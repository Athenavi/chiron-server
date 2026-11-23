-- 平台集成配置：当前用于支付宝。value_enc 为 AES-256-GCM 密文，
-- AAD = platform_setting:<key>；平台 API 永不回显密文或明文密钥。
CREATE TABLE IF NOT EXISTS platform_settings (
    key         text PRIMARY KEY,
    value_enc   text NOT NULL,
    updated_by  uuid REFERENCES platform_admins(id),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
