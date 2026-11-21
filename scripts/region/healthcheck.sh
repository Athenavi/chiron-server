#!/usr/bin/env bash
# P2：区域健康探测与切流建议（只读；不修改 DNS/控制库）。
#
# 必需：
#   PRIMARY_BASE   主区入口，如 https://app.cn-east.example.com
#   STANDBY_BASE   备区入口，如 https://app.cn-east-2.example.com
#   TENANT_HOST    租户 Host，用于通过反代命中数据面
# 可选：
#   TIMEOUT_SECS   单次探测超时（默认 5）
#   MAX_PRIMARY_MS 主区可接受延迟（默认 2000）
#
# 退出码：0=保持主区；10=建议切流；20=两区都不可用。
set -euo pipefail

: "${PRIMARY_BASE:?PRIMARY_BASE is required}"
: "${STANDBY_BASE:?STANDBY_BASE is required}"
: "${TENANT_HOST:?TENANT_HOST is required}"
TIMEOUT_SECS="${TIMEOUT_SECS:-5}"
MAX_PRIMARY_MS="${MAX_PRIMARY_MS:-2000}"

probe() {
  local base="$1"
  local raw
  raw=$(curl -sS --max-time "$TIMEOUT_SECS" -o /dev/null \
    -w '%{http_code} %{time_total}' \
    -H "Host: ${TENANT_HOST}" "${base%/}/readyz" 2>/dev/null || true)
  if [[ -z "$raw" ]]; then
    printf '000 0\n'
  else
    printf '%s\n' "$raw"
  fi
}

primary="$(probe "$PRIMARY_BASE")"
standby="$(probe "$STANDBY_BASE")"
p_code="${primary%% *}"
p_secs="${primary##* }"
s_code="${standby%% *}"
s_secs="${standby##* }"
p_ms=$(awk -v s="$p_secs" 'BEGIN { printf "%d", s * 1000 }')
s_ms=$(awk -v s="$s_secs" 'BEGIN { printf "%d", s * 1000 }')

if [[ "$p_code" == "200" && "$p_ms" -le "$MAX_PRIMARY_MS" ]]; then
  decision=keep
  code=0
elif [[ "$s_code" == "200" ]]; then
  decision=failover
  code=10
else
  decision=unavailable
  code=20
fi

printf 'primary code=%s latency_ms=%s\n' "$p_code" "$p_ms"
printf 'standby code=%s latency_ms=%s\n' "$s_code" "$s_ms"
printf 'decision=%s tenant=%s host=%s\n' "$decision" "$TENANT_HOST" "$TENANT_HOST"
if [[ "$decision" == "failover" ]]; then
  printf 'next: CONFIRM=%s ./scripts/region/failover.sh --apply (then DNS switch)\n' "${TENANT_SLUG:-<slug>}"
fi
exit "$code"
