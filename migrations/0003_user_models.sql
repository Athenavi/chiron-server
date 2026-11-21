-- 用户隐藏模型：只影响前端模型选择，不阻止 API 显式调用。
-- 主键 (user_id, model)：同一模型对同一用户只有一条，覆盖写时整表替换该用户的行。

CREATE TABLE IF NOT EXISTS user_hidden_models (
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tenant_id  smallint NOT NULL DEFAULT 1,
    model      text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, model)
);

CREATE INDEX IF NOT EXISTS ix_user_hidden_models_tenant
    ON user_hidden_models (tenant_id, user_id);
