-- P1-1：套餐超额策略。block（默认）到达月 token/cost 上限即拒绝；
-- allow 允许继续使用，用量仍按实际写入 tenant_usage_daily/账单。
ALTER TABLE plans ADD COLUMN IF NOT EXISTS overage_policy text NOT NULL DEFAULT 'block';
ALTER TABLE plans DROP CONSTRAINT IF EXISTS plans_overage_policy_check;
ALTER TABLE plans ADD CONSTRAINT plans_overage_policy_check
    CHECK (overage_policy IN ('block', 'allow'));
