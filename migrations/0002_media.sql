-- 媒体库（图片附件）：元数据入库，文件落宿主 MEDIA_ROOT/<tenant>/<user>/。
-- 仅允许位图（不含 SVG，避免脚本型 XSS 直出）。

CREATE TABLE IF NOT EXISTS media_assets (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  smallint NOT NULL REFERENCES tenants(id),
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    filename   text NOT NULL,
    mime       text NOT NULL,
    bytes      bigint NOT NULL,
    sha256     text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_media_tenant_time
    ON media_assets (tenant_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_media_user_time
    ON media_assets (user_id, created_at DESC, id DESC);
