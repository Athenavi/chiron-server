#!/usr/bin/env bash
# P0-3：备份恢复演练。把 SOURCE_DSN 的库 pg_dump 后恢复到 TARGET_DSN，并比对关键表。
# TARGET_DSN 必须是独立空库；脚本会 pg_restore --clean，破坏目标库现有数据。
# 用法：
#   SOURCE_DSN='postgresql://.../chiron_t_acme' TARGET_DSN='postgresql://.../restore_check' \
#   ./scripts/backup/restore-drill.sh
set -euo pipefail

: "${SOURCE_DSN:?SOURCE_DSN is required}"
: "${TARGET_DSN:?TARGET_DSN is required}"
if [[ "$SOURCE_DSN" == "$TARGET_DSN" ]]; then
  echo "refusing: SOURCE_DSN and TARGET_DSN are identical" >&2
  exit 2
fi

DUMP_FILE="${DUMP_FILE:-$(mktemp -d)/chiron-restore.dump}"
KEEP_DUMP="${KEEP_DUMP:-0}"
TABLES="${TABLES:-users sessions messages runs media_assets audit_logs}"

# 密码只进环境变量；命令行只放去掉密码的 DSN，避免 ps/proc 泄露。
split_dsn() {
  mapfile -t parts < <(DSN_VALUE="$1" python - <<'PY'
import os, sys, urllib.parse as u
p = u.urlsplit(os.environ["DSN_VALUE"])
pw = u.unquote(p.password or "")
netloc = p.netloc
if "@" in netloc:
    userinfo, host = netloc.rsplit("@", 1)
    user = userinfo.split(":", 1)[0]
    netloc = (user + "@" + host) if user else host
url = u.urlunsplit((p.scheme, netloc, p.path, p.query, p.fragment))
sys.stdout.buffer.write(pw.encode() + b"\n" + url.encode() + b"\n")
PY
)
  printf '%s\n%s\n' "${parts[0]:-}" "${parts[1]:-}"
}
mapfile -t src < <(split_dsn "$SOURCE_DSN")
mapfile -t dst < <(split_dsn "$TARGET_DSN")
SOURCE_PW="${src[0]}"; SOURCE_URL="${src[1]}"
TARGET_PW="${dst[0]}"; TARGET_URL="${dst[1]}"

cleanup() {
  [[ "$KEEP_DUMP" == "1" ]] || rm -f "$DUMP_FILE"
}
trap cleanup EXIT

now_ms() { python -c 'import time; print(int(time.time() * 1000))'; }

table_exists() {
  local pw="$1" url="$2" table="$3"
  local out
  out=$(PGPASSWORD="$pw" psql "$url" -tAc "SELECT to_regclass('public.${table}') IS NOT NULL")
  [[ "$out" == "t" ]]
}

table_sum() {
  local pw="$1" url="$2" table="$3"
  PGPASSWORD="$pw" psql "$url" -tAc "SELECT count(*)::bigint || ',' || coalesce(md5(string_agg(id::text, ',' ORDER BY id)), 'empty') FROM ${table}"
}

dump_start=$(now_ms)
PGPASSWORD="$SOURCE_PW" pg_dump "$SOURCE_URL" --format=custom --no-owner --file="$DUMP_FILE"
chmod 600 "$DUMP_FILE" 2>/dev/null || true
dump_end=$(now_ms)
restore_start=$(now_ms)
PGPASSWORD="$TARGET_PW" pg_restore --clean --if-exists --no-owner --no-privileges --dbname="$TARGET_URL" "$DUMP_FILE"
restore_end=$(now_ms)

failed=0
echo "table                 source                         target                         result"
for table in $TABLES; do
  if ! table_exists "$SOURCE_PW" "$SOURCE_URL" "$table"; then
    echo "$(printf '%-20s' "$table") (missing in source; skipped)"
    continue
  fi
  source_sum=$(table_sum "$SOURCE_PW" "$SOURCE_URL" "$table")
  target_sum=$(table_sum "$TARGET_PW" "$TARGET_URL" "$table")
  if [[ "$source_sum" == "$target_sum" ]]; then
    result=PASS
  else
    result=FAIL
    failed=$((failed + 1))
  fi
  echo "$(printf '%-20s' "$table") $(printf '%-30s' "$source_sum") $(printf '%-30s' "$target_sum") $result"
done

echo "dump_ms=$((dump_end - dump_start))"
echo "restore_ms=$((restore_end - restore_start))"
echo "dump_bytes=$(stat -c %s "$DUMP_FILE")"
if (( failed > 0 )); then
  echo "restore drill FAILED ($failed table(s) mismatched)" >&2
  exit 1
fi
echo "restore drill PASSED"
