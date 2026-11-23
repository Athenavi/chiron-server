//! 支付宝 RSA2（SHA256withRSA）签名与异步通知校验。
//!
//! 只实现本项目需要的 `alipay.trade.page.pay` 与异步通知；密钥由环境变量注入，
//! 不落库、不打印。签名串按参数名排序，排除 `sign`/`sign_type` 与空值。

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::{FixedOffset, Utc};
use rsa::{
    pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey},
    pkcs1v15::{Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey},
    signature::{SignatureEncoding, Signer, Verifier},
    RsaPrivateKey, RsaPublicKey,
};
use sha2::Sha256;
use url::form_urlencoded::Serializer;

pub fn configured(app_id: &str, private_key: &str, public_key: &str, notify_url: &str) -> bool {
    !app_id.trim().is_empty()
        && !private_key.trim().is_empty()
        && !public_key.trim().is_empty()
        && !notify_url.trim().is_empty()
}

fn canonical(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .filter(|(key, value)| {
            !value.is_empty() && key.as_str() != "sign" && key.as_str() != "sign_type"
        })
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn normalize_pem(raw: &str, tag: &str) -> String {
    let raw = raw.trim().replace("\\n", "\n");
    if raw.contains("-----BEGIN") {
        return raw;
    }
    let body: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let wrapped = body
        .as_bytes()
        .chunks(64)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    format!("-----BEGIN {tag}-----\n{wrapped}\n-----END {tag}-----\n")
}

fn private_key(raw: &str) -> Result<RsaPrivateKey, String> {
    RsaPrivateKey::from_pkcs8_pem(&normalize_pem(raw, "PRIVATE KEY"))
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(&normalize_pem(raw, "RSA PRIVATE KEY")))
        .map_err(|e| format!("invalid alipay private key: {e}"))
}

fn public_key(raw: &str) -> Result<RsaPublicKey, String> {
    RsaPublicKey::from_public_key_pem(&normalize_pem(raw, "PUBLIC KEY"))
        .or_else(|_| RsaPublicKey::from_pkcs1_pem(&normalize_pem(raw, "RSA PUBLIC KEY")))
        .map_err(|e| format!("invalid alipay public key: {e}"))
}

pub fn validate_private_key(raw: &str) -> Result<(), String> {
    private_key(raw).map(|_| ())
}

pub fn validate_public_key(raw: &str) -> Result<(), String> {
    public_key(raw).map(|_| ())
}

pub fn verify_notify(public_key_pem: &str, params: &BTreeMap<String, String>) -> bool {
    let Some(sign) = params.get("sign") else {
        return false;
    };
    let cleaned = sign.replace(' ', "+");
    let Ok(signature) = STANDARD.decode(cleaned.trim()) else {
        return false;
    };
    let Ok(key) = public_key(public_key_pem) else {
        return false;
    };
    let Ok(signature) = Signature::try_from(signature.as_slice()) else {
        return false;
    };
    let verifier = VerifyingKey::<Sha256>::new(key);
    verifier
        .verify(canonical(params).as_bytes(), &signature)
        .is_ok()
}

pub fn page_pay_url(
    app_id: &str,
    private_key_pem: &str,
    gateway: &str,
    notify_url: &str,
    return_url: &str,
    out_trade_no: &str,
    total_cents: i64,
    subject: &str,
) -> Result<String, String> {
    if total_cents <= 0 {
        return Err("invoice total must be positive".into());
    }
    let key = private_key(private_key_pem)?;
    let timestamp = Utc::now()
        .with_timezone(&FixedOffset::east_opt(8 * 3600).expect("GMT+8"))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let biz = serde_json::json!({
        "out_trade_no": out_trade_no,
        "total_amount": format!("{}.{:02}", total_cents / 100, total_cents % 100),
        "subject": subject,
        "product_code": "FAST_INSTANT_TRADE_PAY",
    });
    let mut params = BTreeMap::new();
    params.insert("app_id".into(), app_id.trim().to_string());
    params.insert("method".into(), "alipay.trade.page.pay".into());
    params.insert("format".into(), "JSON".into());
    params.insert("charset".into(), "utf-8".into());
    params.insert("sign_type".into(), "RSA2".into());
    params.insert("timestamp".into(), timestamp);
    params.insert("version".into(), "1.0".into());
    params.insert("notify_url".into(), notify_url.trim().to_string());
    if !return_url.trim().is_empty() {
        params.insert("return_url".into(), return_url.trim().to_string());
    }
    params.insert("biz_content".into(), biz.to_string());

    let signing = SigningKey::<Sha256>::new(key);
    let signature = signing.sign(canonical(&params).as_bytes());
    params.insert("sign".into(), STANDARD.encode(signature.to_vec()));

    let mut query = Serializer::new(String::new());
    for (key, value) in &params {
        query.append_pair(key, value);
    }
    Ok(format!(
        "{}?{}",
        gateway.trim_end_matches('?').trim_end_matches('&'),
        query.finish()
    ))
}

pub fn out_trade_no(tenant_id: uuid::Uuid, month: &str) -> String {
    format!(
        "chiron{}{}",
        tenant_id.simple(),
        month.trim().replace('-', "")
    )
}

pub fn parse_out_trade_no(raw: &str) -> Option<(uuid::Uuid, String)> {
    let rest = raw.trim().strip_prefix("chiron")?;
    if rest.len() != 38 {
        return None;
    }
    let (id, month) = rest.split_at(32);
    let tenant = uuid::Uuid::parse_str(id).ok()?;
    if month.len() != 6 {
        return None;
    }
    let year = month.get(0..4)?;
    let mon = month.get(4..6)?;
    Some((tenant, format!("{year}-{mon}")))
}

/// 支付宝异步通知状态映射。
pub fn notify_status(trade_status: &str) -> Option<&'static str> {
    match trade_status.trim().to_ascii_uppercase().as_str() {
        "TRADE_SUCCESS" | "TRADE_FINISHED" => Some("paid"),
        "WAIT_BUYER_PAY" => Some("pending"),
        "TRADE_CLOSED" => Some("failed"),
        _ => None,
    }
}

pub fn yuan_to_cents(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('-') || raw.starts_with('+') {
        return None;
    }
    let (yuan, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if fraction.len() > 2 || !yuan.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let yuan: i64 = yuan.parse().ok()?;
    let cents = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().ok()? * 10,
        _ => fraction.parse::<i64>().ok()?,
    };
    yuan.checked_mul(100)?.checked_add(cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_trade_no_roundtrip() {
        let id = uuid::Uuid::from_u128(42);
        let raw = out_trade_no(id, "2026-11");
        assert_eq!(parse_out_trade_no(&raw), Some((id, "2026-11".into())));
        assert!(parse_out_trade_no("nope").is_none());
    }

    #[test]
    fn alipay_amount_parsing() {
        assert_eq!(yuan_to_cents("0.01"), Some(1));
        assert_eq!(yuan_to_cents("12.3"), Some(1230));
        assert_eq!(yuan_to_cents("12"), Some(1200));
        assert!(yuan_to_cents("-1").is_none());
        assert!(yuan_to_cents("1.234").is_none());
        assert!(yuan_to_cents("abc").is_none());
    }

    #[test]
    fn trade_status_mapping() {
        assert_eq!(notify_status("TRADE_SUCCESS"), Some("paid"));
        assert_eq!(notify_status("WAIT_BUYER_PAY"), Some("pending"));
        assert_eq!(notify_status("X"), None);
    }

    #[test]
    fn generated_pay_url_signature_verifies() {
        use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
        let mut rng = rand::thread_rng();
        let key = RsaPrivateKey::new(&mut rng, 2048).expect("key");
        let private_pem = key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("private pem")
            .to_string();
        let public_pem = key
            .to_public_key()
            .to_public_key_pem(LineEnding::LF)
            .expect("public pem");
        let url = page_pay_url(
            "app-1",
            &private_pem,
            "https://openapi.alipay.com/gateway.do",
            "https://platform.example.com/api/platform/v1/billing/alipay/notify",
            "https://platform.example.com/return",
            "chiron000000000000000000000000000000000000202611",
            5012,
            "Chiron acme 2026-11",
        )
        .expect("pay url");
        let query = url.split_once('?').expect("query").1;
        let params: BTreeMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect();
        assert!(verify_notify(&public_pem, &params));
    }
}
