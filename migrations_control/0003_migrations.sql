-- S3：租户 schema 版本与批量升级记录。
-- 回滚不做自动 down（SQLx 无 down）：按 run 记录定位受影响租户，用按库备份恢复。

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS schema_version bigint NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS migrated_at    timestamptz;

CREATE TABLE IF NOT EXISTS tenant_migration_runs (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_email    text NOT NULL DEFAULT '',
    dry_run        boolean NOT NULL DEFAULT false,
    status         text NOT NULL DEFAULT 'running',
    target_version bigint NOT NULL,
    total          integer NOT NULL DEFAULT 0,
    ok_count       integer NOT NULL DEFAULT 0,
    failed_count   integer NOT NULL DEFAULT 0,
    skipped_count  integer NOT NULL DEFAULT 0,
    started_at     timestamptz NOT NULL DEFAULT now(),
    finished_at    timestamptz,
    detail         jsonb NOT NULL DEFAULT '[]'::jsonb,
    CONSTRAINT tenant_migration_runs_status_check
        CHECK (status IN ('running', 'completed', 'partial', 'failed'))
);
CREATE INDEX IF NOT EXISTS ix_tenant_migration_runs_started
    ON tenant_migration_runs (started_at DESC);
