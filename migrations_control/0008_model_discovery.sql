-- S9：模型自动发现 + 路由健康探测。
--
-- 口径：
-- * `tenant_models.source` 区分目录条目来源：`manual`（管理员手填，默认）与
--   `discovered`（`POST /admin/models/discover?persist=true` 写入）；`discovered_at`
--   记录最近一次发现时间（手工条目为 NULL）。
-- * `tenant_model_routes` 增加探测状态：`last_ok_at` 最近一次成功探测、
--   `last_probe_at` 最近一次探测（无论成败）、`last_error` 最近一次失败原因
--   （脱敏并截断 300 字符，空串表示从未失败/已恢复）。
--
-- 全部语句幂等（IF NOT EXISTS），可重复执行。

ALTER TABLE tenant_models
    ADD COLUMN IF NOT EXISTS source        text NOT NULL DEFAULT 'manual',
    ADD COLUMN IF NOT EXISTS discovered_at timestamptz;

ALTER TABLE tenant_model_routes
    ADD COLUMN IF NOT EXISTS last_ok_at    timestamptz,
    ADD COLUMN IF NOT EXISTS last_error    text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS last_probe_at timestamptz;
