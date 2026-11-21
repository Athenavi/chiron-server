-- S8：租户模型服务（接入/启停/池化）+ 运行时路由。
--
-- 口径：
-- * `tenant_provider_keys` 从"每 provider 一条"升级为 key 池：同一 provider 可按
--   label 挂多条 key，带 weight（加权随机）与 enabled（启停）。旧行保留，归入
--   label='default'，weight=1，enabled=true，因此升级对现网无感。
-- * key 仍是 AES-256-GCM 密文（AAD = `<tenant_uuid>:<provider>`），本表不存明文。
-- * `tenant_models` 是租户可见模型目录；`tenant_model_routes` 把"对外模型名"
--   映射到"上游 provider + upstream_model"，运行时按 priority/weight 选路。
--
-- 全部语句幂等（IF NOT EXISTS / DO $$），可重复执行。

-- 1) key 池化：先补列（老行由默认值填充），再换主键。
ALTER TABLE tenant_provider_keys
    ADD COLUMN IF NOT EXISTS id         uuid NOT NULL DEFAULT gen_random_uuid(),
    ADD COLUMN IF NOT EXISTS label      text NOT NULL DEFAULT 'default',
    ADD COLUMN IF NOT EXISTS weight     integer NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS enabled    boolean NOT NULL DEFAULT true;

-- 老主键 (tenant_id, provider) 必须让位给 id；(tenant_id, provider, label) 由唯一索引保证。
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'tenant_provider_keys'::regclass
           AND conname = 'tenant_provider_keys_pkey'
    ) THEN
        ALTER TABLE tenant_provider_keys DROP CONSTRAINT tenant_provider_keys_pkey;
    ELSIF EXISTS (
        SELECT 1 FROM pg_class
         WHERE relname = 'tenant_provider_keys_pkey'
           AND relkind = 'i'
    ) THEN
        DROP INDEX tenant_provider_keys_pkey;
    END IF;
END $$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'tenant_provider_keys'::regclass
           AND contype = 'p'
    ) THEN
        ALTER TABLE tenant_provider_keys ADD CONSTRAINT tenant_provider_keys_id_pkey PRIMARY KEY (id);
    END IF;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS ux_tenant_provider_keys_pool
    ON tenant_provider_keys (tenant_id, provider, label);

CREATE INDEX IF NOT EXISTS ix_tenant_provider_keys_tenant_provider
    ON tenant_provider_keys (tenant_id, provider)
    WHERE enabled;

-- 2) 租户模型目录（对普通用户可见的模型清单）。
CREATE TABLE IF NOT EXISTS tenant_models (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    model        text NOT NULL,
    display_name text NOT NULL DEFAULT '',
    enabled      boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_tenant_models_model
    ON tenant_models (tenant_id, model);
CREATE INDEX IF NOT EXISTS ix_tenant_models_enabled
    ON tenant_models (tenant_id)
    WHERE enabled;

-- 3) 模型路由：对外模型名 → 上游 provider / upstream_model。
CREATE TABLE IF NOT EXISTS tenant_model_routes (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    model          text NOT NULL,
    provider       text NOT NULL,
    upstream_model text NOT NULL,
    priority       integer NOT NULL DEFAULT 100,
    weight         integer NOT NULL DEFAULT 1,
    enabled        boolean NOT NULL DEFAULT true,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_tenant_model_routes_key
    ON tenant_model_routes (tenant_id, model, provider);
CREATE INDEX IF NOT EXISTS ix_tenant_model_routes_lookup
    ON tenant_model_routes (tenant_id, model, priority)
    WHERE enabled;
