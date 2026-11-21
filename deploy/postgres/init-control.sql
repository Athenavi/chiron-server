-- 初始化时创建控制库（只在 pgdata 卷为空时执行一次）。
-- 租户库由 chiron-server 开通流程按需创建，不在这里建。
CREATE DATABASE chiron_control;
