-- 平台管理员 TOTP 二次验证（仅平台侧；租户用户认证不受影响）。
-- 密钥以 AES-256-GCM 密文落库，AAD 为 `platform_totp:<admin_id>`；只在 setup 响应里明文出现一次。

ALTER TABLE platform_admins ADD COLUMN IF NOT EXISTS totp_secret_enc text;
ALTER TABLE platform_admins ADD COLUMN IF NOT EXISTS totp_enabled boolean NOT NULL DEFAULT false;
