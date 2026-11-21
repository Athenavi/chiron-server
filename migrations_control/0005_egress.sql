-- S7（可落地部分）：租户 egress 策略。bridge = 允许出网（默认），none = 完全断网。
-- 细粒度域名白名单/凭证哨兵需要独立 egress 代理，暂不在单节点内实现。

ALTER TABLE plans
    ADD COLUMN IF NOT EXISTS egress_mode text NOT NULL DEFAULT 'bridge';
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'plans_egress_mode_check'
    ) THEN
        ALTER TABLE plans
            ADD CONSTRAINT plans_egress_mode_check CHECK (egress_mode IN ('bridge', 'none'));
    END IF;
END $$;
