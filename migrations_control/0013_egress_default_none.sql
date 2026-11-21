-- 生产安全默认：bridge 模式没有租户间网络隔离，可访问宿主机/元数据。
-- 新套餐默认 none；已有 bridge 套餐收敛为 none，需要出网请显式改 proxy（白名单）。
ALTER TABLE plans ALTER COLUMN egress_mode SET DEFAULT 'none';
UPDATE plans SET egress_mode = 'none' WHERE egress_mode = 'bridge';
