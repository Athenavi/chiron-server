-- S5：租户 BYOK 与套餐模型/成本上限。
-- key 以 AES-256-GCM 加密存储（AAD = tenant_id:provider），只允许平台侧写入，接口不回明文。

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS model_allowlist    text[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS monthly_cost_cents bigint NOT NULL DEFAULT 0;

CREATE TABLE IF NOT EXISTS tenant_provider_keys (
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    provider   text NOT NULL,
    key_enc    text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, provider)
);
