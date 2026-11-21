//! 控制库敏感字段的静态加密（AES-256-GCM）。
//!
//! 只加密落库的 DSN 与 owner 临时密码；密钥来自 `SECRETS_KEY`（base64，32 字节）。
//! AAD 绑定租户 id，密文挪到别的租户行会解密失败。

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;

pub const KEY_ENV: &str = "SECRETS_KEY";

pub fn decode_key(raw: &str) -> Result<[u8; 32], String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{KEY_ENV} is not set"));
    }
    let bytes = STANDARD
        .decode(raw)
        .map_err(|_| format!("{KEY_ENV} must be base64"))?;
    if bytes.len() != 32 {
        return Err(format!("{KEY_ENV} must decode to exactly 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    Ok(key)
}

pub fn seal(key_raw: &str, aad: &[u8], plaintext: &str) -> Result<String, String> {
    let key = decode_key(key_raw)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher init: {e}"))?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad,
            },
        )
        .map_err(|_| "encrypt failed".to_string())?;
    let mut out = Vec::with_capacity(nonce.len() + sealed.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    Ok(STANDARD.encode(out))
}

pub fn open(key_raw: &str, aad: &[u8], encoded: &str) -> Result<String, String> {
    let key = decode_key(key_raw)?;
    let raw = STANDARD
        .decode(encoded.trim())
        .map_err(|_| "ciphertext is not base64".to_string())?;
    if raw.len() < 13 {
        return Err("ciphertext is truncated".into());
    }
    let (nonce, body) = raw.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher init: {e}"))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad })
        .map_err(|_| "decrypt failed (wrong key or tampered ciphertext)".to_string())?;
    String::from_utf8(plain).map_err(|_| "plaintext is not utf-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;

    fn key() -> String {
        STANDARD.encode([7u8; 32])
    }

    #[test]
    fn roundtrip_and_aad_binding() {
        let k = key();
        let sealed = seal(&k, b"tenant-a", "postgresql://u:p@localhost/db").unwrap();
        assert_eq!(
            open(&k, b"tenant-a", &sealed).unwrap(),
            "postgresql://u:p@localhost/db"
        );
        assert!(open(&k, b"tenant-b", &sealed).is_err());
    }

    #[test]
    fn rejects_bad_keys_and_tampering() {
        assert!(decode_key("").is_err());
        assert!(decode_key("not-base64!!").is_err());
        assert!(decode_key(&STANDARD.encode([0u8; 16])).is_err());

        let k = key();
        let sealed = seal(&k, b"", "secret").unwrap();
        let mut raw = STANDARD.decode(&sealed).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0xff;
        assert!(open(&k, b"", &STANDARD.encode(raw)).is_err());
    }
}
