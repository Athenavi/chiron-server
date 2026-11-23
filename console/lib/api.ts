export type Plan = {
  id: string
  name: string
  isolation_tier: 'sysbox' | 'gvisor' | 'microvm'
  egress_mode?: 'bridge' | 'proxy' | 'none'
  egress_allowlist?: string[]
  model_allowlist?: string[]
  monthly_cost_cents?: number
  overage_policy?: 'block' | 'allow'
  max_users: number
  max_concurrent_runs: number
  monthly_tokens: number
  max_storage_gb: number
  max_sandboxes: number
  price_cents: number
}

export type Tenant = {
  id: string
  slug: string
  name: string
  status: string
  plan_id: string
  region: string
  owner_email: string
  db_name: string
  provision_error: string
  provisioned_at: string | null
  created_at: string
  updated_at: string
}

export type Domain = {
  id: string
  tenant_id: string
  host: string
  kind: 'subdomain' | 'custom'
  verified_at: string | null
  tls_status: string
}

export type TenantVersion = {
  id: string
  slug: string
  status: string
  schema_version: number
  migrated_at: string | null
}

export type MigrationRun = {
  id: string
  actor_email: string
  dry_run: boolean
  status: string
  target_version: number
  total: number
  ok_count: number
  failed_count: number
  skipped_count: number
  started_at: string
  finished_at: string | null
}

export type MigrationStatus = {
  target_version: number
  tenants: TenantVersion[]
  runs: MigrationRun[]
}

export type MigrationDetail = {
  id: string
  slug: string
  from: number
  to: number
  status: 'ok' | 'failed' | 'skipped'
  error?: string
  pending?: boolean
}

export type UpgradeResult = {
  run_id: string
  dry_run: boolean
  target_version: number
  status: string
  total: number
  ok_count: number
  failed_count: number
  skipped_count: number
  detail: MigrationDetail[]
}

export type UsageRow = {
  tenant_id: string
  slug: string | null
  day: string
  runs: number
  tokens_in: number
  tokens_out: number
  cost_cents: number
  storage_bytes: number
}

export type UsageRollup = {
  days: number
  tenants_ok: number
  tenants_failed: number
  rows: number
  detail: { id: string; slug: string; days?: number; error?: string }[]
}

export type ProviderKey = {
  provider: string
  updated_at: string
}

export type Invoice = {
  number: string
  month: string
  plan_id: string
  base_cents: number
  usage_cents: number
  total_cents: number
  payment: { provider: string; status: string }
  lines: { kind: string; description: string; amount_cents: number }[]
}

export type BillReconcile = {
  month: string
  drift: boolean
  rollup: { tokens_in: number; tokens_out: number; cost_cents: number; runs: number }
  tenant_db: { tokens_in: number; tokens_out: number; cost_cents: number; runs: number }
}

export type AlipaySettings = {
  app_id: string
  notify_url: string
  return_url: string
  gateway: string
  has_private_key: boolean
  has_public_key: boolean
  configured: boolean
  stored_in_db: string[]
  secrets_key_configured: boolean
}

export type AlipaySettingsBody = {
  app_id?: string
  private_key?: string
  public_key?: string
  notify_url?: string
  return_url?: string
  gateway?: string
}

export type AlertSettings = {
  webhook_configured: boolean
  cooldown_secs: number
  active_runs: number
  tenant_pools: number
  egress_denied: number
}

export type RatePolicyView = { burst: number; per_minute: number }

export type RateSettings = {
  enabled: boolean
  login: RatePolicyView
  submit: RatePolicyView
  tool: RatePolicyView
  heavy: RatePolicyView
}

export type MediaSettings = {
  backend: string
  webdav: {
    url: string
    username: string
    prefix: string
    has_password: boolean
  }
  s3: {
    bucket: string
    region: string
    endpoint: string | null
    path_style: boolean | null
    prefix: string | null
    has_access_key: boolean
    has_secret_key: boolean
    has_session_token: boolean
  }
}

export type ProviderEntry = {
  id: string
  label: string
  requires_key: boolean
  configured: boolean
  stored_in_db: boolean
}

export type PlatformMe = {
  id: string
  email: string
  totp_enabled: boolean
}

export type TotpSetup = {
  secret: string
  otpauth_url: string
}

// 生产默认同源（由反代转发 /api/platform），避免浏览器请求访问者的 localhost。
const DEV_PROXY = process.env.NEXT_PUBLIC_DEV_PROXY === '1'
const BASE =
  process.env.NEXT_PUBLIC_PLATFORM_API_BASE ||
  (process.env.NODE_ENV === 'production' || DEV_PROXY ? '' : 'http://127.0.0.1:8088')
const TOKEN_KEY = 'chiron_platform_token'
// 生产靠 httpOnly 平台 cookie；sessionStorage Bearer 只服务跨端口本地开发。
const PERSIST_TOKEN = process.env.NODE_ENV !== 'production'

export function getToken(): string {
  if (!PERSIST_TOKEN || typeof window === 'undefined') return ''
  return sessionStorage.getItem(TOKEN_KEY) || ''
}

export function setToken(token: string) {
  if (!PERSIST_TOKEN || typeof window === 'undefined') return
  if (token) sessionStorage.setItem(TOKEN_KEY, token)
  else sessionStorage.removeItem(TOKEN_KEY)
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Content-Type', 'application/json')
  const token = getToken()
  if (token) headers.set('Authorization', `Bearer ${token}`)
  const res = await fetch(`${BASE}${path}`, {
    ...init,
    headers,
    credentials: 'include',
    cache: 'no-store',
  })
  const body = await res.json().catch(() => ({}))
  if (!res.ok) {
    const message = body?.error?.message || body?.error || `HTTP ${res.status}`
    const err = new Error(message) as Error & { status?: number; code?: string }
    err.status = res.status
    err.code = body?.error?.code
    throw err
  }
  return (body?.data ?? body) as T
}

export async function downloadTenantExport(id: string, filename: string) {
  const headers = new Headers()
  const token = getToken()
  if (token) headers.set('Authorization', `Bearer ${token}`)
  const res = await fetch(`${BASE}/api/platform/v1/tenants/${id}/export`, {
    method: 'POST',
    headers,
    credentials: 'include',
    cache: 'no-store',
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body?.error?.message || `HTTP ${res.status}`)
  }
  const blob = await res.blob()
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}

export const api = {
  login: (email: string, password: string, totpCode?: string) =>
    request<{ token: string; admin: { id: string; email: string } }>(
      '/api/platform/v1/auth/login',
      {
        method: 'POST',
        body: JSON.stringify(
          totpCode ? { email, password, totp_code: totpCode } : { email, password },
        ),
      },
    ),
  me: () => request<PlatformMe>('/api/platform/v1/me'),
  totpSetup: () =>
    request<TotpSetup>('/api/platform/v1/me/totp/setup', { method: 'POST' }),
  totpEnable: (code: string) =>
    request<{ totp_enabled: boolean; recovery_codes: string[] }>(
      '/api/platform/v1/me/totp/enable',
      { method: 'POST', body: JSON.stringify({ code }) },
    ),
  totpDisable: (code: string) =>
    request<{ totp_enabled: boolean }>('/api/platform/v1/me/totp/disable', {
      method: 'POST',
      body: JSON.stringify({ code }),
    }),
  plans: () => request<{ plans: Plan[] }>('/api/platform/v1/plans').then(r => r.plans),
  tenants: () => request<{ tenants: Tenant[] }>('/api/platform/v1/tenants').then(r => r.tenants),
  tenant: (id: string) => request<{ tenant: Tenant }>(`/api/platform/v1/tenants/${id}`).then(r => r.tenant),
  createTenant: (body: {
    slug: string
    name: string
    plan_id: string
    region?: string
    owner_email?: string
  }) => request<Tenant>('/api/platform/v1/tenants', { method: 'POST', body: JSON.stringify(body) }),
  updateTenant: (id: string, body: { name?: string; plan_id?: string }) =>
    request<{ tenant: Tenant }>(`/api/platform/v1/tenants/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(body),
    }).then(r => r.tenant),
  suspend: (id: string) =>
    request<{ tenant: Tenant }>(`/api/platform/v1/tenants/${id}/suspend`, { method: 'POST' }).then(r => r.tenant),
  resume: (id: string) =>
    request<{ tenant: Tenant }>(`/api/platform/v1/tenants/${id}/resume`, { method: 'POST' }).then(r => r.tenant),
  provision: (id: string) =>
    request<{ tenant: Tenant }>(`/api/platform/v1/tenants/${id}/provision`, { method: 'POST' }).then(r => r.tenant),
  ownerCredentials: (id: string) =>
    request<{ email: string; password: string }>(
      `/api/platform/v1/tenants/${id}/owner-credentials`,
      { method: 'POST' },
    ),
  domains: (id: string) =>
    request<{ domains: Domain[] }>(`/api/platform/v1/tenants/${id}/domains`).then(r => r.domains),
  addDomain: (id: string, host: string) =>
    request<Domain>(`/api/platform/v1/tenants/${id}/domains`, {
      method: 'POST',
      body: JSON.stringify({ host }),
    }),
  probeDomainTls: (id: string, domainId: string) =>
    request<{ domain_id: string; host: string; tls_status: string; error: string }>(
      `/api/platform/v1/tenants/${id}/domains/${domainId}/probe-tls`,
      { method: 'POST' },
    ),
  invoice: (id: string, month: string) =>
    request<Invoice>(
      `/api/platform/v1/tenants/${id}/invoice?month=${encodeURIComponent(month)}`,
    ),
  reconcileBill: (id: string, month: string) =>
    request<BillReconcile>(
      `/api/platform/v1/tenants/${id}/bill/reconcile?month=${encodeURIComponent(month)}`,
    ),
  alipaySettings: () =>
    request<{ alipay: AlipaySettings }>('/api/platform/v1/settings/integrations/alipay').then(
      r => r.alipay,
    ),
  saveAlipaySettings: (body: AlipaySettingsBody) =>
    request<{ alipay: AlipaySettings }>('/api/platform/v1/settings/integrations/alipay', {
      method: 'PUT',
      body: JSON.stringify(body),
    }).then(r => r.alipay),
  alertSettings: () => request<AlertSettings>('/api/platform/v1/settings/alerts'),
  saveAlertSettings: (body: Record<string, string | number>) =>
    request<AlertSettings>('/api/platform/v1/settings/alerts', {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  rateSettings: () => request<RateSettings>('/api/platform/v1/settings/rate-limits'),
  saveRateSettings: (body: Record<string, unknown>) =>
    request<RateSettings>('/api/platform/v1/settings/rate-limits', {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  mediaSettings: () => request<MediaSettings>('/api/platform/v1/settings/media'),
  saveMediaSettings: (body: Record<string, unknown>) =>
    request<MediaSettings>('/api/platform/v1/settings/media', {
      method: 'PUT',
      body: JSON.stringify(body),
    }),
  providerSettings: () =>
    request<{ providers: ProviderEntry[] }>('/api/platform/v1/settings/providers').then(
      r => r.providers,
    ),
  saveProviderKey: (provider: string, apiKey: string) =>
    request<{ providers: ProviderEntry[] }>('/api/platform/v1/settings/providers', {
      method: 'PUT',
      body: JSON.stringify({ keys: { [provider]: apiKey } }),
    }).then(r => r.providers),
  verifyDomain: (id: string, domainId: string) =>
    request<{ domain: Domain }>(
      `/api/platform/v1/tenants/${id}/domains/${domainId}/verify`,
      { method: 'POST' },
    ).then(r => r.domain),
  providerKeys: (id: string) =>
    request<{ keys: ProviderKey[] }>(`/api/platform/v1/tenants/${id}/provider-keys`).then(r => r.keys),
  putProviderKey: (id: string, provider: string, apiKey: string) =>
    request<{ provider: string; updated: boolean }>(`/api/platform/v1/tenants/${id}/provider-keys`, {
      method: 'PUT',
      body: JSON.stringify({ provider, api_key: apiKey }),
    }),
  deleteProviderKey: (id: string, provider: string) =>
    request<{ deleted: boolean }>(
      `/api/platform/v1/tenants/${id}/provider-keys/${encodeURIComponent(provider)}`,
      { method: 'DELETE' },
    ),
  deleteTenant: (id: string, confirm: string, backupBefore = true) =>
    request<{ tenant_id: string; database_dropped: string; backup_file: string | null }>(
      `/api/platform/v1/tenants/${id}/delete`,
      { method: 'POST', body: JSON.stringify({ confirm, backup_before: backupBefore }) },
    ),
  backupTenant: (id: string) =>
    request<{ file: string; bytes: number }>(`/api/platform/v1/tenants/${id}/backup`, {
      method: 'POST',
    }),
  migrations: () => request<MigrationStatus>('/api/platform/v1/migrations'),
  usage: () => request<{ usage: UsageRow[] }>('/api/platform/v1/usage').then(r => r.usage),
  rollupUsage: (days: number) =>
    request<UsageRollup>('/api/platform/v1/usage/rollup', {
      method: 'POST',
      body: JSON.stringify({ days }),
    }),
  upgradeMigrations: (dryRun: boolean) =>
    request<UpgradeResult>('/api/platform/v1/migrations/upgrade', {
      method: 'POST',
      body: JSON.stringify({ dry_run: dryRun }),
    }),
}
