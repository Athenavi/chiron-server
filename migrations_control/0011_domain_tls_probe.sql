-- R-5：TLS 状态探测时间。状态本身复用 tenant_domains.tls_status。
ALTER TABLE tenant_domains
    ADD COLUMN IF NOT EXISTS tls_checked_at timestamptz;

CREATE INDEX IF NOT EXISTS ix_tenant_domains_tls_pending
    ON tenant_domains (host)
    WHERE tls_status <> 'issued';
