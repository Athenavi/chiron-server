#!/usr/bin/env bash
# 跨租户边界审计（只读）。用 A 租户的 token 去打 B / 未知 / 暂停租户的 Host，
# 以及反向组合，输出 pass/fail 矩阵。
#
# 只发 GET（可选用 POST /auth/login 取 token），不创建/删除任何数据。
# 不打印 token、密码、响应体；失败时只显示 HTTP 状态与稳定错误码。
# 用法与环境变量见同目录 README.md。

set -uo pipefail

BASE="${AUDIT_BASE_URL:-}"
if [ -z "$BASE" ]; then
  echo "错误：必须设置 AUDIT_BASE_URL（租户 A 的 origin，如 https://acme.apps.example.com）" >&2
  exit 2
fi
BASE="${BASE%/}"
if ! printf '%s' "$BASE" | grep -Eq '^https?://[^/]+'; then
  echo "错误：AUDIT_BASE_URL 必须是 http(s)://host[:port] 形式" >&2
  exit 2
fi

HOST_A="${AUDIT_HOST_A:-$(printf '%s' "$BASE" | sed -E 's#^https?://([^/]+).*#\1#')}"
HOST_B="${AUDIT_HOST_B:-}"
HOST_UNKNOWN="${AUDIT_HOST_UNKNOWN:-}"
HOST_SUSPENDED="${AUDIT_HOST_SUSPENDED:-}"
PLATFORM_HOST="${AUDIT_PLATFORM_HOST:-}"

TOKEN_A="${AUDIT_TOKEN_A:-}"
TOKEN_B="${AUDIT_TOKEN_B:-}"
PLATFORM_TOKEN="${AUDIT_PLATFORM_TOKEN:-}"
IDENT_A="${AUDIT_IDENTIFIER_A:-}"
PASS_A="${AUDIT_PASSWORD_A:-}"

SESSION_B_ID="${AUDIT_SESSION_B_ID:-}"
MEDIA_B_ID="${AUDIT_MEDIA_B_ID:-}"

DRY_RUN="${AUDIT_DRY_RUN:-0}"
ALLOW_LOGIN="${AUDIT_ALLOW_LOGIN:-0}"
INSECURE="${AUDIT_INSECURE:-0}"
VERBOSE="${AUDIT_VERBOSE:-0}"
TIMEOUT="${AUDIT_TIMEOUT:-10}"

EXPECT_UNKNOWN_STATUS="${AUDIT_EXPECT_UNKNOWN_STATUS:-404}"
EXPECT_UNKNOWN_CODE="${AUDIT_EXPECT_UNKNOWN_CODE:-not_found}"
EXPECT_SUSPENDED_STATUS="${AUDIT_EXPECT_SUSPENDED_STATUS:-503}"
EXPECT_SUSPENDED_CODE="${AUDIT_EXPECT_SUSPENDED_CODE:-tenant_suspended}"

BODY="$(mktemp "${TMPDIR:-/tmp}/chiron-audit.XXXXXX")"
trap 'rm -f "$BODY"' EXIT

PASS=0
FAIL=0
SKIP=0

say() { printf '%s\n' "$*"; }
hr() { printf '%s\n' "--------------------------------------------------------------------------------"; }

mask() {
  # 只暴露长度，避免 token 进终端/CI 日志。
  if [ -z "$1" ]; then printf '<unset>'; else printf '<set:len=%s>' "${#1}"; fi
}

json_code() {
  # 从统一错误包络里取 error.code；取不到输出 "-"。
  if [ -s "$BODY" ]; then
    local code
    code="$(sed -n 's/.*"code"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$BODY" | head -n 1)"
    if [ -n "$code" ]; then printf '%s' "$code"; return; fi
  fi
  printf '-'
}

json_token() {
  sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$BODY" | head -n 1
}

curl_base() {
  CURL=(curl -sS --max-time "$TIMEOUT" -o "$BODY" -w '%{http_code}')
  if [ "$INSECURE" = "1" ]; then CURL+=(-k); fi
  if [ -n "${AUDIT_CURL_EXTRA:-}" ]; then
    # 允许传 --resolve 等额外参数（空白分隔）。
    # shellcheck disable=SC2206
    CURL+=(${AUDIT_CURL_EXTRA})
  fi
}

record() {
  # record <num> <case> <host> <token-label> <expected> <actual> <result>
  printf '%-3s %-38s %-32s %-14s %-20s %-20s %s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7"
}

probe() {
  local num="$1" name="$2" method="$3" path="$4" host="$5" token="$6" expect_status="$7" expect_code="$8"
  local extra_header="${9:-}"
  local expect="${expect_status}/${expect_code}"
  local token_label="none"
  if [ -n "$token" ]; then
    if [ "$token" = "$TOKEN_A" ]; then
      token_label="A"
    elif [ -n "$TOKEN_B" ] && [ "$token" = "$TOKEN_B" ]; then
      token_label="B"
    elif [ -n "$PLATFORM_TOKEN" ] && [ "$token" = "$PLATFORM_TOKEN" ]; then
      token_label="platform"
    else
      token_label="?"
    fi
  fi

  if [ "$DRY_RUN" = "1" ]; then
    SKIP=$((SKIP + 1))
    record "$num" "$name" "$host" "$token_label" "$expect" "(dry-run)" "SKIP"
    return 0
  fi

  local args=("${CURL[@]}" -X "$method" -H "Host: $host" -H 'Accept: application/json')
  if [ -n "$token" ]; then
    args+=(-H "Authorization: Bearer $token")
  fi
  if [ -n "$extra_header" ]; then
    args+=(-H "$extra_header")
  fi

  : > "$BODY"
  local status
  status="$("${args[@]}" "$BASE$path" 2>/dev/null || true)"
  local code
  code="$(json_code)"

  local result="FAIL"
  if [ "$status" = "$expect_status" ] && [ "$code" = "$expect_code" ]; then
    result="PASS"
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
  fi
  record "$num" "$name" "$host" "$token_label" "$expect" "$status/$code" "$result"

  if [ "$result" = "FAIL" ] && [ "$VERBOSE" = "1" ]; then
    say "      期望 $expect_status/$expect_code，实际 ${status:-000}/$code"
    say "      提示：确认该 Host 已在 tenant_domains 注册且状态与预期一致；3xx 通常表示用了 http:// 基址。"
  fi
}

login_if_needed() {
  if [ -n "$TOKEN_A" ]; then
    return 0
  fi
  if [ -z "$IDENT_A" ] || [ -z "$PASS_A" ]; then
    return 1
  fi
  if [ "$ALLOW_LOGIN" != "1" ]; then
    say "跳过登录：设置了 AUDIT_IDENTIFIER_A/AUDIT_PASSWORD_A，但 AUDIT_ALLOW_LOGIN 不是 1（避免不必要的 POST）。"
    return 1
  fi
  if [ "$DRY_RUN" = "1" ]; then
    TOKEN_A="dry-run-placeholder"
    return 0
  fi
  login "租户 A" "$HOST_A" "$IDENT_A" "$PASS_A"
}

login() {
  local label="$1" host="$2" ident="$3" pass="$4"
  curl_base
  local payload
  payload="$(printf '{"identifier":"%s","password":"%s"}' "$(json_escape "$ident")" "$(json_escape "$pass")")"
  local status
  status="$("${CURL[@]}" -X POST -H "Host: $host" -H 'Content-Type: application/json' \
    --data-binary "$payload" "$BASE/api/v1/auth/login" 2>/dev/null || true)"
  if [ "$status" != "200" ]; then
    say "登录失败（$label）：HTTP ${status:-000}；不重试以免触发登录锁定。"
    return 1
  fi
  local token
  token="$(json_token)"
  if [ -z "$token" ]; then
    say "登录响应缺少 data.token（$label）。"
    return 1
  fi
  if [ "$label" = "租户 A" ]; then TOKEN_A="$token"; else TOKEN_B="$token"; fi
  return 0
}

json_escape() {
  printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
}

# ---------------------------------------------------------------------------

if ! login_if_needed; then
  if [ -z "$TOKEN_A" ]; then
    echo "错误：需要 AUDIT_TOKEN_A，或 AUDIT_IDENTIFIER_A + AUDIT_PASSWORD_A + AUDIT_ALLOW_LOGIN=1" >&2
    exit 2
  fi
fi

if [ -z "$HOST_B" ]; then
  say "警告：未设置 AUDIT_HOST_B，跨租户用例会跳过（边界审计至少要两个租户）。"
fi
if [ -z "$HOST_UNKNOWN" ]; then
  say "警告：未设置 AUDIT_HOST_UNKNOWN，未知 Host 用例会跳过（建议用 *.invalid 之类不会注册的域名）。"
fi

curl_base

say "chiron 跨租户边界审计（只读）"
say "基址：$BASE"
say "Host A：$HOST_A   Host B：${HOST_B:-<unset>}   未知：${HOST_UNKNOWN:-<unset>}   暂停：${HOST_SUSPENDED:-<unset>}"
say "Token A：$(mask "$TOKEN_A")   Token B：$(mask "$TOKEN_B")   Platform token：$(mask "$PLATFORM_TOKEN")"
if [ "$DRY_RUN" = "1" ]; then say "模式：dry-run（不发请求）"; fi
hr
printf '%-3s %-38s %-32s %-14s %-20s %-20s %s\n' "#" "用例" "Host" "token" "期望 状态/错误码" "实际 状态/错误码" "结果"
hr

probe 1 "基线：A token + A Host" GET /api/v1/auth/session "$HOST_A" "$TOKEN_A" 200 -
probe 2 "无 token + A Host" GET /api/v1/sessions "$HOST_A" "" 401 auth.required
probe 3 "A token + A Host（大写/尾点归一化）" GET /api/v1/auth/session \
  "$(printf '%s' "${HOST_A%:*}" | tr '[:lower:]' '[:upper:]')." "$TOKEN_A" 200 -
probe 4 "A token + A Host（Host 带端口）" GET /api/v1/auth/session "${HOST_A%:*}:443" "$TOKEN_A" 200 -
probe 5 "A token + X-Forwarded-Host 伪造 B" GET /api/v1/auth/session "$HOST_A" "$TOKEN_A" 200 - \
  "X-Forwarded-Host: ${HOST_B:-evil.invalid}"

if [ -n "$HOST_B" ]; then
  probe 6 "A token + B Host" GET /api/v1/auth/session "$HOST_B" "$TOKEN_A" 401 auth.required
  probe 7 "无 token + B Host" GET /api/v1/sessions "$HOST_B" "" 401 auth.required
  if [ -n "${AUDIT_IDENTIFIER_B:-}" ] && [ -n "${AUDIT_PASSWORD_B:-}" ] \
    && [ "$ALLOW_LOGIN" = "1" ] && [ "$DRY_RUN" != "1" ]; then
    login "租户 B" "$HOST_B" "$AUDIT_IDENTIFIER_B" "$AUDIT_PASSWORD_B" || true
  fi
  if [ -n "$TOKEN_B" ]; then
    probe 8 "B token + A Host" GET /api/v1/auth/session "$HOST_A" "$TOKEN_B" 401 auth.required
    probe 9 "B token + B Host（基线）" GET /api/v1/auth/session "$HOST_B" "$TOKEN_B" 200 -
  else
    SKIP=$((SKIP + 2)); record 8 "B token + A Host" "$HOST_A" "B" "401/auth.required" "(no token B)" "SKIP"
    record 9 "B token + B Host（基线）" "$HOST_B" "B" "200/-" "(no token B)" "SKIP"
  fi
else
  SKIP=$((SKIP + 4))
  record 6 "A token + B Host" "<unset>" "A" "401/auth.required" "(skipped)" "SKIP"
  record 7 "无 token + B Host" "<unset>" "none" "401/auth.required" "(skipped)" "SKIP"
  record 8 "B token + A Host" "$HOST_A" "B" "401/auth.required" "(skipped)" "SKIP"
  record 9 "B token + B Host（基线）" "<unset>" "B" "200/-" "(skipped)" "SKIP"
fi

if [ -n "$HOST_UNKNOWN" ]; then
  probe 10 "A token + 未知 Host" GET /api/v1/auth/session "$HOST_UNKNOWN" "$TOKEN_A" \
    "$EXPECT_UNKNOWN_STATUS" "$EXPECT_UNKNOWN_CODE"
  probe 11 "无 token + 未知 Host" GET /api/v1/sessions "$HOST_UNKNOWN" "" \
    "$EXPECT_UNKNOWN_STATUS" "$EXPECT_UNKNOWN_CODE"
else
  SKIP=$((SKIP + 2))
  record 10 "A token + 未知 Host" "<unset>" "A" "$EXPECT_UNKNOWN_STATUS/$EXPECT_UNKNOWN_CODE" "(skipped)" "SKIP"
  record 11 "无 token + 未知 Host" "<unset>" "none" "$EXPECT_UNKNOWN_STATUS/$EXPECT_UNKNOWN_CODE" "(skipped)" "SKIP"
fi

if [ -n "$HOST_SUSPENDED" ]; then
  probe 12 "A token + 暂停租户 Host" GET /api/v1/auth/session "$HOST_SUSPENDED" "$TOKEN_A" \
    "$EXPECT_SUSPENDED_STATUS" "$EXPECT_SUSPENDED_CODE"
  probe 13 "无 token + 暂停租户 Host" GET /api/v1/sessions "$HOST_SUSPENDED" "" \
    "$EXPECT_SUSPENDED_STATUS" "$EXPECT_SUSPENDED_CODE"
else
  SKIP=$((SKIP + 2))
  record 12 "A token + 暂停租户 Host" "<unset>" "A" "$EXPECT_SUSPENDED_STATUS/$EXPECT_SUSPENDED_CODE" "(skipped)" "SKIP"
  record 13 "无 token + 暂停租户 Host" "<unset>" "none" "$EXPECT_SUSPENDED_STATUS/$EXPECT_SUSPENDED_CODE" "(skipped)" "SKIP"
fi

probe 14 "平台 API + 租户 A Host" GET /api/platform/v1/me "$HOST_A" "$TOKEN_A" 404 not_found
probe 15 "平台 API + 未知 Host" GET /api/platform/v1/me "${HOST_UNKNOWN:-$HOST_A}" "" \
  "$EXPECT_UNKNOWN_STATUS" "$EXPECT_UNKNOWN_CODE"

if [ -n "$PLATFORM_HOST" ]; then
  probe 16 "租户 A token + 平台 Host" GET /api/v1/auth/session "$PLATFORM_HOST" "$TOKEN_A" 404 not_found
  probe 17 "平台 API + 平台 Host（无 token）" GET /api/platform/v1/me "$PLATFORM_HOST" "" 401 auth.required
  if [ -n "$PLATFORM_TOKEN" ]; then
    probe 18 "平台 token + 租户 A Host" GET /api/platform/v1/me "$HOST_A" "$PLATFORM_TOKEN" 404 not_found
  else
    SKIP=$((SKIP + 1)); record 18 "平台 token + 租户 A Host" "$HOST_A" "P" "404/not_found" "(no token)" "SKIP"
  fi
else
  SKIP=$((SKIP + 3))
  record 16 "租户 A token + 平台 Host" "<unset>" "A" "404/not_found" "(skipped)" "SKIP"
  record 17 "平台 API + 平台 Host（无 token）" "<unset>" "none" "401/auth.required" "(skipped)" "SKIP"
  record 18 "平台 token + 租户 A Host" "$HOST_A" "P" "404/not_found" "(skipped)" "SKIP"
fi

if [ -n "$SESSION_B_ID" ]; then
  probe 19 "A token + A Host 读 B 的会话" GET "/api/v1/sessions/$SESSION_B_ID" "$HOST_A" "$TOKEN_A" 404 not_found
else
  SKIP=$((SKIP + 1)); record 19 "A token + A Host 读 B 的会话" "$HOST_A" "A" "404/not_found" "(no id)" "SKIP"
fi

if [ -n "$MEDIA_B_ID" ]; then
  probe 20 "A token + A Host 读 B 的媒体" GET "/api/v1/media/$MEDIA_B_ID" "$HOST_A" "$TOKEN_A" 404 not_found
  if [ -n "$HOST_B" ]; then
    probe 21 "A token + B Host 读 B 的媒体" GET "/api/v1/media/$MEDIA_B_ID" "$HOST_B" "$TOKEN_A" 401 auth.required
  else
    SKIP=$((SKIP + 1)); record 21 "A token + B Host 读 B 的媒体" "<unset>" "A" "401/auth.required" "(skipped)" "SKIP"
  fi
else
  SKIP=$((SKIP + 2))
  record 20 "A token + A Host 读 B 的媒体" "$HOST_A" "A" "404/not_found" "(no id)" "SKIP"
  record 21 "A token + B Host 读 B 的媒体" "${HOST_B:-<unset>}" "A" "401/auth.required" "(skipped)" "SKIP"
fi

hr
say "通过 $PASS，失败 $FAIL，跳过 $SKIP"
say ""
say "审计留痕：拒绝发生在认证/租户中间件，不会写租户审计表；请到反代 JSON 访问日志"
say "（Caddy: /var/log/caddy/chiron-access.log）按 Host + 状态码核对这批请求，并把日志接入告警。"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
exit 0
