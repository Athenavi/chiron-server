#!/usr/bin/env bash
# 主备区域切流（R-3）。默认只打印计划；--apply 需要 CONFIRM=<tenant-slug>。
#
# 必需环境变量：
#   PLATFORM_BASE   https://platform.example.com
#   PLATFORM_TOKEN  平台管理员 Bearer token
#   TENANT_ID       控制库租户 uuid
#   TENANT_SLUG     仅用于二次确认，不打印 DSN
#   TENANT_HOST     租户 Host（用于目标区 /healthz 校验）
#   TARGET_REGION   目标区域标识（与目标区 CHIRON_REGION 一致）
#   SOURCE_DSN      源租户库 DSN（pg_dump）
#   TARGET_DSN      目标租户库 DSN（pg_restore；必须已存在且为空/可覆盖）
# 可选：
#   TARGET_BASE     目标区服务端地址（默认 https://platform.example.com）
#   DUMP_FILE       临时 dump 路径（默认 /tmp/chiron-<slug>.dump）
#
# 本脚本不做 DNS 切换；切换前先跑 plan，确认备份、目标库和回滚点。
set -euo pipefail

MODE="${1:-plan}"

need() {
  local name="$1"
  [[ -n "${!name:-}" ]] || { echo "missing env: $name" >&2; exit 2; }
}
for var in PLATFORM_BASE PLATFORM_TOKEN TENANT_ID TENANT_SLUG TENANT_HOST TARGET_REGION SOURCE_DSN TARGET_DSN; do
  need "$var"
done

DUMP_FILE="${DUMP_FILE:-/tmp/chiron-${TENANT_SLUG}.dump}"
TARGET_BASE="${TARGET_BASE:-$PLATFORM_BASE}"
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

# 把 DSN 拆成「密码」和「去掉密码的 URL」：密码只进环境变量，避免出现在 ps/proc。
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

api() {
  curl -fsS \
    -H "Authorization: Bearer ${PLATFORM_TOKEN}" \
    -H 'Content-Type: application/json' \
    "$@"
}

echo "plan: tenant=${TENANT_SLUG} ${TENANT_ID}"
echo "plan: source -> target region ${TARGET_REGION}"
echo "plan: dump ${DUMP_FILE}"
echo "plan: steps: suspend -> pg_dump -> pg_restore -> PATCH region -> resume -> verify"
echo "plan: DNS switch is manual; keep the old region read-only until verification passes"
echo "plan: started ${started}"

if [[ "$MODE" != "--apply" ]]; then
  echo "dry-run only; re-run with --apply and CONFIRM=${TENANT_SLUG}"
  exit 0
fi
if [[ "${CONFIRM:-}" != "$TENANT_SLUG" ]]; then
  echo "refusing to apply: set CONFIRM=${TENANT_SLUG}" >&2
  exit 2
fi
if [[ "$SOURCE_DSN" == "$TARGET_DSN" ]]; then
  echo "refusing: SOURCE_DSN and TARGET_DSN are identical" >&2
  exit 2
fi

cleanup() {
  [[ -f "$DUMP_FILE" ]] && rm -f "$DUMP_FILE"
}
trap cleanup EXIT

echo "1/6 suspend"
api -X POST "${PLATFORM_BASE}/api/platform/v1/tenants/${TENANT_ID}/suspend" >/dev/null

echo "2/6 dump source tenant database"
PGPASSWORD="$SOURCE_PW" pg_dump --format=custom --no-owner --file="$DUMP_FILE" "$SOURCE_URL"

echo "3/6 restore into target tenant database"
PGPASSWORD="$TARGET_PW" pg_restore --clean --if-exists --no-owner --no-privileges \
  --dbname="$TARGET_URL" "$DUMP_FILE"

echo "4/6 move control-plane region and DSN"
# DSN 通过 stdin 传给 curl，避免出现在进程列表/历史里。
PATCH_BODY=$(python - <<'PY'
import json, os
print(json.dumps({"region": os.environ["TARGET_REGION"], "dsn": os.environ["TARGET_DSN"]}))
PY
)
printf '%s' "$PATCH_BODY" | api -X PATCH \
  "${PLATFORM_BASE}/api/platform/v1/tenants/${TENANT_ID}" \
  -H 'Content-Type: application/json' --data-binary @- >/dev/null

echo "5/7 verify target region health"
curl -fsS -H "Host: ${TENANT_HOST}" "${TARGET_BASE}/healthz" >/dev/null

if [[ -n "${DNS_SWITCH_HOOK:-}" ]]; then
  echo "6/7 call DNS switch hook"
  HOOK_BODY=$(python - <<'PY'
import json, os
print(json.dumps({
    "tenant_id": os.environ["TENANT_ID"],
    "tenant_slug": os.environ["TENANT_SLUG"],
    "host": os.environ["TENANT_HOST"],
    "target_region": os.environ["TARGET_REGION"],
    "target_base": os.environ.get("TARGET_BASE", ""),
}))
PY
)
  printf '%s' "$HOOK_BODY" | curl -fsS -X POST "${DNS_SWITCH_HOOK}" \
    -H 'Content-Type: application/json' --data-binary @- >/dev/null
else
  echo "6/7 DNS switch hook not configured (manual step)"
fi

echo "7/7 resume"
api -X POST "${PLATFORM_BASE}/api/platform/v1/tenants/${TENANT_ID}/resume" >/dev/null

finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "applied: started=${started} finished=${finished}"
echo "next: switch DNS/反代 to target region, then run tenant login + SSE + media checks"
