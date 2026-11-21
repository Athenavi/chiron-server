#!/usr/bin/env bash
# P2：健康探测驱动的区域切流观察器。
#
# 默认只演练：连续 ROUNDS 次判定 failover 后打印将要执行的命令并退出 10。
# 真正执行必须同时满足：
#   --apply、AUTO_FAILOVER=1、CONFIRM=<tenant-slug>
# 其余切流变量（PLATFORM_BASE/TOKEN、DSN 等）沿用 failover.sh。
set -euo pipefail

MODE=plan
if [[ "${1:-}" == "--apply" ]]; then
  MODE=apply
fi
ROUNDS="${ROUNDS:-3}"
SLEEP_SECS="${SLEEP_SECS:-30}"
MAX_PRIMARY_MS="${MAX_PRIMARY_MS:-2000}"

: "${PRIMARY_BASE:?PRIMARY_BASE is required}"
: "${STANDBY_BASE:?STANDBY_BASE is required}"
: "${TENANT_HOST:?TENANT_HOST is required}"

here="$(cd "$(dirname "$0")" && pwd)"
streak=0
last_code=0
for round in $(seq 1 "$ROUNDS"); do
  set +e
  MAX_PRIMARY_MS="$MAX_PRIMARY_MS" TENANT_HOST="$TENANT_HOST" \
    PRIMARY_BASE="$PRIMARY_BASE" STANDBY_BASE="$STANDBY_BASE" \
    bash "$here/healthcheck.sh"
  code=$?
  set -e
  last_code=$code
  if [[ "$code" -eq 10 ]]; then
    streak=$((streak + 1))
  else
    streak=0
  fi
  echo "auto-failover round=${round}/${ROUNDS} decision_exit=${code} streak=${streak}"
  if [[ "$streak" -lt "$ROUNDS" && "$round" -lt "$ROUNDS" ]]; then
    sleep "$SLEEP_SECS"
  fi
done

if [[ "$streak" -lt "$ROUNDS" ]]; then
  echo "auto-failover: no action (last_exit=${last_code})"
  exit "$last_code"
fi

if [[ "$MODE" != "apply" ]]; then
  echo "auto-failover: would run CONFIRM=${TENANT_SLUG:-<slug>} $here/failover.sh --apply"
  exit 10
fi
if [[ "${AUTO_FAILOVER:-}" != "1" ]]; then
  echo "auto-failover: refusing to apply without AUTO_FAILOVER=1" >&2
  exit 2
fi
if [[ -z "${TENANT_SLUG:-}" || "${CONFIRM:-}" != "$TENANT_SLUG" ]]; then
  echo "auto-failover: set CONFIRM=<tenant-slug> to apply" >&2
  exit 2
fi

exec bash "$here/failover.sh" --apply
