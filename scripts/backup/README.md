# 备份恢复演练（P0-3）

`restore-drill.sh` 把源租户库 `pg_dump` 后恢复到独立目标库，并比对关键表的行数与
id 哈希。用于验证 `CHIRON_BACKUP_DIR` 里的备份真的可恢复，而不是只看备份文件大小。

## 使用

```bash
SOURCE_DSN='postgresql://user@host/chiron_t_acme' \
TARGET_DSN='postgresql://user@host/restore_check' \
./scripts/backup/restore-drill.sh
```

- `TARGET_DSN` 必须是独立空库；脚本执行 `pg_restore --clean --if-exists`，会覆盖目标库。
- 默认校验 `users/sessions/messages/runs/media_assets/audit_logs`；可用 `TABLES` 覆盖。
- 默认删除临时 dump；`KEEP_DUMP=1` 保留用于事后分析。
- 不打印 DSN；失败时返回非零，可接发布门禁。

## 本机演练记录（2026-11-22）

临时 PostgreSQL + 迁移 + 1 用户/会话/消息/run：

| 表 | 结果 |
| :--- | :--- |
| users / sessions / messages / runs | 行数与 id 哈希一致 |
| media_assets / audit_logs | 空表一致 |
| dump / restore | 334 ms / 500 ms（68 KB dump） |

6 张关键表全部 PASS，脚本退出码 0。

正式环境应按最大租户数据量演练，并把 RTO 写入值班手册；本脚本只验证“备份可恢复”，
不替代生产网络/磁盘/权限演练。
