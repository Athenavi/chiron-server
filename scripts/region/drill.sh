#!/usr/bin/env bash
# R-3 本地 dump/restore 演练：临时集群 -> migrations -> N 行数据 -> dump -> restore -> 校验。
# 只测数据库恢复 RTO，不模拟 DNS/反代/JWT；真实双区演练仍见 README。
set -euo pipefail

export PGCLIENTENCODING=UTF8
ROWS="${ROWS:-50000}"
PORT="${PORT:-55433}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d)"
DATA="$TMP/data"
LOG="$TMP/pg.log"
DUMP="$TMP/source.dump"

cleanup() {
  pg_ctl -D "$DATA" stop -m fast >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT

initdb -D "$DATA" -U postgres -A trust --no-locale --encoding=UTF8 >/dev/null
pg_ctl -D "$DATA" -o "-p $PORT -h 127.0.0.1" -l "$LOG" start >/dev/null
for _ in $(seq 1 30); do
  pg_isready -h 127.0.0.1 -p "$PORT" -U postgres >/dev/null 2>&1 && break
  sleep 1
done
pg_isready -h 127.0.0.1 -p "$PORT" -U postgres >/dev/null

PSQL="psql -h 127.0.0.1 -p $PORT -U postgres"
$PSQL -v ON_ERROR_STOP=1 -c "CREATE DATABASE r3_source" >/dev/null
$PSQL -v ON_ERROR_STOP=1 -c "CREATE DATABASE r3_target" >/dev/null
for migration in "$ROOT"/migrations/*.sql; do
  $PSQL -v ON_ERROR_STOP=1 -d r3_source -f "$migration" >/dev/null
done
$PSQL -v ON_ERROR_STOP=1 -d r3_source -c \
  "CREATE TABLE r3_probe(id bigint PRIMARY KEY, payload text NOT NULL);
   INSERT INTO r3_probe
   SELECT g, md5(g::text) || repeat('x', 80) FROM generate_series(1, $ROWS) g;" >/dev/null

source_sum=$($PSQL -d r3_source -tAc \
  "SELECT count(*) || ',' || coalesce(sum(hashtext(payload)), 0) FROM r3_probe")
now_ms() { python -c 'import time; print(int(time.time() * 1000))'; }

dump_start=$(now_ms)
pg_dump -h 127.0.0.1 -p "$PORT" -U postgres -Fc -f "$DUMP" r3_source
dump_end=$(now_ms)
restore_start=$(now_ms)
pg_restore -h 127.0.0.1 -p "$PORT" -U postgres --clean --if-exists \
  --no-owner --no-privileges -d r3_target "$DUMP"
restore_end=$(now_ms)

target_sum=$($PSQL -d r3_target -tAc \
  "SELECT count(*) || ',' || coalesce(sum(hashtext(payload)), 0) FROM r3_probe")

echo "rows=$ROWS"
echo "dump_ms=$((dump_end - dump_start))"
echo "restore_ms=$((restore_end - restore_start))"
echo "dump_bytes=$(stat -c %s "$DUMP")"
echo "source_checksum=$source_sum"
echo "target_checksum=$target_sum"
if [[ "$source_sum" != "$target_sum" ]]; then
  echo "checksum=MISMATCH" >&2
  exit 1
fi
echo "checksum=match"
