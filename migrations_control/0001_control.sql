-- 控制库：租户元数据、域名、套餐、平台管理员、平台审计。
-- 业务数据在每租户独立库；这里只存路由与运营所需的最小事实。

CREATE TABLE IF NOT EXISTS platform_admins (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email         text NOT NULL,
    password_hash text NOT NULL,
    status        text NOT NULL DEFAULT 'active',
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT platform_admins_status_check CHECK (status IN ('active', 'disabled'))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_platform_admins_email ON platform_admins (lower(email));

CREATE TABLE IF NOT EXISTS plans (
    id                 text PRIMARY KEY,
    name               text NOT NULL,
    isolation_tier     text NOT NULL,
    max_users          integer NOT NULL DEFAULT 20,
    max_concurrent_runs integer NOT NULL DEFAULT 2,
    monthly_tokens     bigint NOT NULL DEFAULT 10000000,
    max_storage_gb     integer NOT NULL DEFAULT 20,
    max_sandboxes      integer NOT NULL DEFAULT 20,
    price_cents        integer NOT NULL DEFAULT 0,
    created_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT plans_tier_check CHECK (isolation_tier IN ('sysbox', 'gvisor', 'microvm'))
);

INSERT INTO plans (id, name, isolation_tier, max_users, max_concurrent_runs,
                   monthly_tokens, max_storage_gb, max_sandboxes, price_cents)
VALUES
    ('standard',  'Standard',  'sysbox',  20,  2,  10000000,  20,  20,  0),
    ('pro',       'Pro',       'gvisor',  100, 10, 100000000, 200, 100, 0),
    ('enterprise','Enterprise','microvm', 1000,50, 1000000000,2000,1000, 0)
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS tenants (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug        text NOT NULL,
    name        text NOT NULL,
    status      text NOT NULL DEFAULT 'pending',
    plan_id     text NOT NULL REFERENCES plans(id),
    region      text NOT NULL DEFAULT 'default',
    owner_email text NOT NULL DEFAULT '',
    db_name     text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tenants_status_check CHECK (status IN ('pending', 'provisioning', 'active', 'suspended', 'failed', 'deleted'))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_tenants_slug ON tenants (lower(slug));
CREATE UNIQUE INDEX IF NOT EXISTS ux_tenants_db_name ON tenants (db_name);

CREATE TABLE IF NOT EXISTS tenant_domains (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    host        text NOT NULL,
    kind        text NOT NULL DEFAULT 'subdomain',
    verified_at timestamptz,
    tls_status  text NOT NULL DEFAULT 'pending',
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tenant_domains_kind_check CHECK (kind IN ('subdomain', 'custom'))
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_tenant_domains_host ON tenant_domains (lower(host));
-- 每租户最多 1 个自定义域名。
CREATE UNIQUE INDEX IF NOT EXISTS ux_tenant_domains_one_custom
    ON tenant_domains (tenant_id) WHERE kind = 'custom';

CREATE TABLE IF NOT EXISTS tenant_usage_daily (
    tenant_id    uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    day          date NOT NULL,
    runs         bigint NOT NULL DEFAULT 0,
    tokens_in    bigint NOT NULL DEFAULT 0,
    tokens_out   bigint NOT NULL DEFAULT 0,
    cost_cents   bigint NOT NULL DEFAULT 0,
    storage_bytes bigint NOT NULL DEFAULT 0,
    PRIMARY KEY (tenant_id, day)
);

CREATE TABLE IF NOT EXISTS platform_audit (
    id         bigserial PRIMARY KEY,
    actor_id   uuid,
    actor_email text NOT NULL DEFAULT '',
    action     text NOT NULL,
    tenant_id  uuid,
    detail     text NOT NULL DEFAULT '',
    ip         text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_platform_audit_time ON platform_audit (created_at DESC);
CREATE INDEX IF NOT EXISTS ix_platform_audit_tenant ON platform_audit (tenant_id, created_at DESC);
