-- P1-1：外部支付系统回写发票状态。金额以分为单位，provider/external_id 由支付侧提供。
CREATE TABLE IF NOT EXISTS tenant_invoice_status (
    tenant_id     uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    month         text NOT NULL,
    provider      text NOT NULL DEFAULT '',
    external_id   text NOT NULL DEFAULT '',
    status        text NOT NULL DEFAULT 'pending',
    amount_cents  bigint NOT NULL DEFAULT 0,
    paid_at       timestamptz,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, month),
    CONSTRAINT tenant_invoice_status_check
        CHECK (status IN ('pending', 'paid', 'failed', 'refunded'))
);
