-- P1-2：记录平台管理员最近成功使用的 TOTP 时间步，拒绝 ±1 窗口内的重放。
ALTER TABLE platform_admins
    ADD COLUMN IF NOT EXISTS totp_last_step bigint;
