-- P1-2：平台管理员 TOTP 一次性恢复码（HMAC-SHA256 后落库，明文只在启用响应出现一次）。
CREATE TABLE IF NOT EXISTS platform_admin_recovery_codes (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    admin_id   uuid NOT NULL REFERENCES platform_admins(id) ON DELETE CASCADE,
    code_hash  text NOT NULL,
    used_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_platform_admin_recovery_unused
    ON platform_admin_recovery_codes (admin_id)
    WHERE used_at IS NULL;
