-- S1：租户库开通状态。dsn_enc 与 owner_temp_password_enc 为 AES-256-GCM
-- （SECRETS_KEY）密文；明文只在开通/一次性领取时短暂存在于进程内。

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS db_user              text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS dsn_enc              text,
    ADD COLUMN IF NOT EXISTS provisioned_at       timestamptz,
    ADD COLUMN IF NOT EXISTS provision_error      text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS owner_temp_password_enc text;
