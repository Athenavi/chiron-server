//! `web_fetch`：由 Rust 侧发起，不经过用户容器，因此必须自己做 SSRF 防护。
//!
//! 规则（PRD §7.3 / §9）：
//! - 只允许 http/https，拒绝带 userinfo 的 URL；
//! - 主机名命中 `localhost`/`.local`/`.internal` 等直接拒；
//! - 解析出的所有地址必须都是公网地址（回环、私网、链路本地、CGNAT、多播全拒）；
//! - 重定向手动跟随（最多 3 跳），每一跳重新做同一套校验；
//! - 响应体按字节上限截断，不把大文件读进内存。

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use futures_util::StreamExt;
use serde::Serialize;
use url::Url;

use super::error::AgentError;

/// 默认响应体上限（1 MiB）。
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const REQUEST_TIMEOUT_SECS: u64 = 20;

const BLOCKED_HOST_SUFFIXES: &[&str] = &[".local", ".internal", ".localhost", ".lan", ".home"];
const BLOCKED_HOSTS: &[&str] = &["localhost", "metadata", "metadata.google.internal"];

#[derive(Clone, Debug, Serialize)]
pub struct WebPage {
    pub url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
    pub truncated: bool,
}

#[derive(Clone)]
pub struct WebClient {
    client: reqwest::Client,
    /// 测试/内网部署可显式放行私网地址（默认拒绝）。
    allow_private: bool,
    max_bytes: usize,
}

impl std::fmt::Debug for WebClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebClient")
            .field("allow_private", &self.allow_private)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl Default for WebClient {
    fn default() -> Self {
        Self::new(false)
    }
}

impl WebClient {
    pub fn new(allow_private: bool) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("chiron-server/web_fetch")
            .build()
            .unwrap_or_default();
        Self {
            client,
            allow_private,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    pub fn allow_private_from_env() -> bool {
        matches!(
            std::env::var("CHIRON_WEB_FETCH_ALLOW_PRIVATE")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "1" | "true" | "yes" | "on"
        )
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(1024);
        self
    }

    pub async fn fetch(&self, raw_url: &str) -> Result<WebPage, AgentError> {
        let mut url = Url::parse(raw_url.trim())
            .map_err(|e| AgentError::Tool(format!("invalid url: {e}")))?;
        let mut hops = 0;
        loop {
            let pin = self.check_url(&url).await?;
            let client = self.client_for(pin.as_ref())?;
            let response = client
                .get(url.clone())
                .send()
                .await
                .map_err(|e| AgentError::Tool(format!("web_fetch failed: {e}")))?;

            if response.status().is_redirection() {
                if hops >= MAX_REDIRECTS {
                    return Err(AgentError::Tool("too many redirects".into()));
                }
                hops += 1;
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| AgentError::Tool("redirect without location".into()))?;
                url = url
                    .join(location)
                    .map_err(|e| AgentError::Tool(format!("invalid redirect: {e}")))?;
                continue;
            }

            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let (bytes, truncated) = self.read_capped(response).await;
            return Ok(WebPage {
                url: url.to_string(),
                status,
                content_type,
                body: String::from_utf8_lossy(&bytes).into_owned(),
                truncated,
            });
        }
    }

    async fn read_capped(&self, response: reqwest::Response) -> (Vec<u8>, bool) {
        let mut stream = response.bytes_stream();
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let room = self.max_bytes.saturating_sub(out.len());
                    if room == 0 {
                        return (out, true);
                    }
                    if bytes.len() > room {
                        out.extend_from_slice(&bytes[..room]);
                        return (out, true);
                    }
                    out.extend_from_slice(&bytes);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "web_fetch body stream failed");
                    break;
                }
            }
        }
        (out, false)
    }

    /// 校验通过后把域名固定到已校验的 IP，避免请求阶段二次解析被 DNS rebinding 绕过。
    fn client_for(&self, pin: Option<&(String, SocketAddr)>) -> Result<reqwest::Client, AgentError> {
        let Some((host, addr)) = pin else {
            return Ok(self.client.clone());
        };
        reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("chiron-server/web_fetch")
            .resolve(host, *addr)
            .build()
            .map_err(|e| AgentError::Tool(format!("web_fetch client failed: {e}")))
    }

    /// 逐跳校验：先做字面检查，再做 DNS 解析后的地址检查。
    /// 返回 `Some((host, addr))` 时调用方应把该域名固定到该地址。
    pub async fn check_url(&self, url: &Url) -> Result<Option<(String, SocketAddr)>, AgentError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AgentError::Tool(
                "web_fetch only supports http/https".into(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AgentError::Tool(
                "web_fetch does not accept credentials in urls".into(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| AgentError::Tool("url has no host".into()))?;
        let host_lower = host.trim_end_matches('.').to_ascii_lowercase();
        let blocked_host = !self.allow_private
            && (BLOCKED_HOSTS.contains(&host_lower.as_str())
                || BLOCKED_HOST_SUFFIXES
                    .iter()
                    .any(|suffix| host_lower.ends_with(suffix)));
        if blocked_host {
            return Err(AgentError::Tool(format!(
                "web_fetch refused private host {host_lower}"
            )));
        }

        // 字面量 IP 直接按地址规则判定；域名则解析后逐地址判定。
        if let Ok(ip) = host_lower.trim_matches(['[', ']']).parse::<IpAddr>() {
            self.check_ip(ip, &host_lower)?;
            return Ok(None);
        }
        let port = url.port_or_known_default().unwrap_or(80);
        let addrs = tokio::net::lookup_host((host_lower.as_str(), port))
            .await
            .map_err(|e| AgentError::Tool(format!("dns lookup failed for {host_lower}: {e}")))?;
        let mut chosen = None;
        for addr in addrs {
            self.check_ip(addr.ip(), &host_lower)?;
            chosen.get_or_insert(addr);
        }
        let addr = chosen.ok_or_else(|| {
            AgentError::Tool(format!("dns lookup returned no address for {host_lower}"))
        })?;
        Ok(Some((host.to_string(), addr)))
    }

    fn check_ip(&self, ip: IpAddr, host: &str) -> Result<(), AgentError> {
        if self.allow_private {
            return Ok(());
        }
        if is_forbidden_ip(ip) {
            return Err(AgentError::Tool(format!(
                "web_fetch refused private address for {host}"
            )));
        }
        Ok(())
    }
}

/// 回环/私网/链路本地/CGNAT/多播/未指定地址一律禁止。
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                || (v4.octets()[0] == 100 && (64..=127).contains(&v4.octets()[1]))
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
                || (v4.octets()[0] == 192 && v4.octets()[1] == 88 && v4.octets()[2] == 99)
                || (v4.octets()[0] == 198 && matches!(v4.octets()[1], 18 | 19))
                || v4.octets()[0] >= 240
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(mapped));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || (seg[0] & 0xffc0) == 0xfec0 // site local
                || (seg[0] == 0x2001 && seg[1] == 0) // Teredo embeds IPv4
                || seg[0] == 0x2002 // 6to4 embeds IPv4
                || (seg[0] == 0x0064 && seg[1] == 0xff9b) // NAT64
        }
    }
}

/// 给工具层用的共享句柄。
pub type SharedWebClient = Arc<WebClient>;

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).expect("url")
    }

    #[test]
    fn forbidden_ip_ranges_cover_the_usual_ssrf_targets() {
        for raw in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "192.88.99.1",
        ] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(is_forbidden_ip(ip), "{raw} must be forbidden");
        }
        for raw in ["::1", "fc00::1", "fe80::1", "2001:db8::1", "2002:a9fe:a9fe::1"] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(is_forbidden_ip(ip), "{raw} must be forbidden");
        }
        assert!(is_forbidden_ip(IpAddr::V6(
            Ipv6Addr::from(0x1u128) // ::1
        )));
        assert!(is_forbidden_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
    }

    #[test]
    fn public_addresses_are_allowed() {
        for raw in ["1.1.1.1", "93.184.216.34", "2606:2800:220:1::1"] {
            let ip: IpAddr = raw.parse().unwrap();
            assert!(!is_forbidden_ip(ip), "{raw} should be allowed");
        }
    }

    #[tokio::test]
    async fn schemes_and_credentials_are_rejected() {
        let client = WebClient::new(false);
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "http://user:pass@example.com/",
        ] {
            let err = client.check_url(&url(bad)).await.expect_err(bad);
            assert!(matches!(err, AgentError::Tool(_)), "{bad}");
        }
    }

    #[tokio::test]
    async fn localhost_and_private_literals_are_rejected() {
        let client = WebClient::new(false);
        for bad in [
            "http://localhost:8080/",
            "http://foo.internal/",
            "http://127.0.0.1/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
        ] {
            assert!(client.check_url(&url(bad)).await.is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn allow_private_opt_in_bypasses_the_guard() {
        let client = WebClient::new(true);
        client
            .check_url(&url("http://127.0.0.1:9/"))
            .await
            .expect("opt-in allows loopback");
    }

    #[tokio::test]
    async fn fetch_reads_and_caps_body() {
        let router =
            axum::Router::new().route("/big", axum::routing::get(|| async { "x".repeat(4096) }));
        let base = crate::provider::testutil::spawn(router).await;
        let client = WebClient::new(true).with_max_bytes(1024);
        let page = client.fetch(&format!("{base}/big")).await.expect("fetch");
        assert_eq!(page.status, 200);
        assert_eq!(page.body.len(), 1024);
        assert!(page.truncated);
    }
}
