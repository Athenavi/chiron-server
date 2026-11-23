import { expect, test, type Page, type Route } from '@playwright/test'

type Json = Record<string, unknown>

const tenant = {
  id: 't1',
  slug: 'acme',
  name: 'Acme Inc.',
  status: 'suspended',
  plan_id: 'standard',
  region: 'default',
  owner_email: 'owner@acme.test',
  db_name: 'chiron_t_acme',
  provision_error: '',
  provisioned_at: null,
  created_at: new Date(0).toISOString(),
  updated_at: new Date(0).toISOString(),
}

function json(route: Route, body: Json, status = 200) {
  return route.fulfill({
    status,
    contentType: 'application/json',
    body: JSON.stringify(body),
  })
}

async function mockConsoleApi(page: Page) {
  let totpEnabled = false
  let tlsStatus = 'pending'
  let alipay = {
    app_id: '',
    notify_url: '',
    return_url: '',
    gateway: 'https://openapi.alipay.com/gateway.do',
    has_private_key: false,
    has_public_key: false,
    configured: false,
    stored_in_db: [] as string[],
    secrets_key_configured: true,
  }
  let alerts = {
    webhook_configured: false,
    cooldown_secs: 1800,
    active_runs: 0,
    tenant_pools: 0,
    egress_denied: 0,
  }
  let rates = {
    enabled: true,
    login: { burst: 10, per_minute: 10 },
    submit: { burst: 10, per_minute: 30 },
    tool: { burst: 30, per_minute: 120 },
    heavy: { burst: 4, per_minute: 20 },
  }
  let media = {
    backend: 'local',
    webdav: { url: '', username: '', prefix: '', has_password: false },
    s3: {
      bucket: '',
      region: '',
      endpoint: null as string | null,
      path_style: null as boolean | null,
      prefix: null as string | null,
      has_access_key: false,
      has_secret_key: false,
      has_session_token: false,
    },
  }
  let providers = [
    { id: 'openai', label: 'OpenAI', requires_key: true, configured: false, stored_in_db: false },
    { id: 'anthropic', label: 'Anthropic', requires_key: true, configured: false, stored_in_db: false },
  ]
  await page.route('**/api/platform/v1/**', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    const path = url.pathname
    const method = request.method()

    if (path === '/api/platform/v1/auth/login' && method === 'POST') {
      const body = request.postDataJSON() as { totp_code?: string }
      if (!body.totp_code) {
        return json(route, { error: { code: 'auth.totp_required', message: 'totp required' } }, 401)
      }
      return json(route, {
        data: { token: 'e2e-token', admin: { id: 'admin-1', email: 'ops@acme.test' } },
      })
    }
    if (path === '/api/platform/v1/me' && method === 'GET') {
      return json(route, {
        data: { id: 'admin-1', email: 'ops@acme.test', totp_enabled: totpEnabled },
      })
    }
    if (path === '/api/platform/v1/me/totp/setup' && method === 'POST') {
      return json(route, {
        data: { secret: 'JBSWY3DPEHPK3PXP', otpauth_url: 'otpauth://totp/Chiron?secret=JBSWY3DPEHPK3PXP' },
      })
    }
    if (path === '/api/platform/v1/me/totp/enable' && method === 'POST') {
      totpEnabled = true
      return json(route, {
        data: { totp_enabled: true, recovery_codes: ['AAAAA-BBBBB', 'CCCCC-DDDDD'] },
      })
    }
    if (path === '/api/platform/v1/plans' && method === 'GET') {
      return json(route, {
        data: {
          plans: [
            {
              id: 'standard',
              name: 'Standard',
              isolation_tier: 'sysbox',
              max_users: 20,
              max_concurrent_runs: 5,
              monthly_tokens: 1000000,
              monthly_cost_cents: 5000,
              max_storage_gb: 20,
              max_sandboxes: 5,
              price_cents: 5000,
            },
          ],
        },
      })
    }
    if (path === '/api/platform/v1/tenants' && method === 'GET') {
      return json(route, { data: { tenants: [] } })
    }
    if (path === '/api/platform/v1/tenants' && method === 'POST') {
      return json(route, { data: tenant })
    }
    if (path === '/api/platform/v1/settings/integrations/alipay' && method === 'GET') {
      return json(route, { data: { alipay } })
    }
    if (path === '/api/platform/v1/settings/integrations/alipay' && method === 'PUT') {
      const body = request.postDataJSON() as Record<string, string>
      alipay = {
        ...alipay,
        app_id: body.app_id ?? alipay.app_id,
        notify_url: body.notify_url ?? alipay.notify_url,
        return_url: body.return_url ?? alipay.return_url,
        gateway: body.gateway ?? alipay.gateway,
        has_private_key: alipay.has_private_key || Boolean(body.private_key),
        has_public_key: alipay.has_public_key || Boolean(body.public_key),
        configured: true,
        stored_in_db: ['alipay_app_id', 'alipay_notify_url'],
      }
      return json(route, { data: { alipay } })
    }
    if (path === '/api/platform/v1/settings/alerts' && method === 'GET') {
      return json(route, { data: alerts })
    }
    if (path === '/api/platform/v1/settings/alerts' && method === 'PUT') {
      const body = request.postDataJSON() as Record<string, string | number>
      alerts = {
        ...alerts,
        webhook_configured: Boolean(body.webhook_url) || alerts.webhook_configured,
        cooldown_secs: Number(body.cooldown_secs ?? alerts.cooldown_secs),
        active_runs: Number(body.active_runs ?? alerts.active_runs),
        tenant_pools: Number(body.tenant_pools ?? alerts.tenant_pools),
        egress_denied: Number(body.egress_denied ?? alerts.egress_denied),
      }
      return json(route, { data: alerts })
    }
    if (path === '/api/platform/v1/settings/rate-limits' && method === 'GET') {
      return json(route, { data: rates })
    }
    if (path === '/api/platform/v1/settings/rate-limits' && method === 'PUT') {
      const body = request.postDataJSON() as Record<string, number>
      rates = {
        enabled: true,
        login: { burst: body.login_burst ?? rates.login.burst, per_minute: body.login_per_minute ?? rates.login.per_minute },
        submit: { burst: body.submit_burst ?? rates.submit.burst, per_minute: body.submit_per_minute ?? rates.submit.per_minute },
        tool: { burst: body.tool_burst ?? rates.tool.burst, per_minute: body.tool_per_minute ?? rates.tool.per_minute },
        heavy: { burst: body.heavy_burst ?? rates.heavy.burst, per_minute: body.heavy_per_minute ?? rates.heavy.per_minute },
      }
      return json(route, { data: rates })
    }
    if (path === '/api/platform/v1/settings/media' && method === 'GET') {
      return json(route, { data: media })
    }
    if (path === '/api/platform/v1/settings/media' && method === 'PUT') {
      const body = request.postDataJSON() as Record<string, unknown>
      media = {
        ...media,
        backend: String(body.backend ?? media.backend),
        webdav: {
          ...media.webdav,
          url: String(body.webdav_url ?? media.webdav.url),
          username: String(body.webdav_username ?? media.webdav.username),
          prefix: String(body.webdav_prefix ?? media.webdav.prefix),
          has_password: media.webdav.has_password || Boolean(body.webdav_password),
        },
      }
      return json(route, { data: media })
    }
    if (path === '/api/platform/v1/settings/providers' && method === 'GET') {
      return json(route, { data: { providers } })
    }
    if (path === '/api/platform/v1/settings/providers' && method === 'PUT') {
      const body = request.postDataJSON() as { keys?: Record<string, string> }
      for (const [id, key] of Object.entries(body.keys ?? {})) {
        providers = providers.map(item =>
          item.id === id ? { ...item, configured: Boolean(key), stored_in_db: Boolean(key) } : item,
        )
      }
      return json(route, { data: { providers } })
    }
    if (path === '/api/platform/v1/tenants/t1' && method === 'GET') {
      return json(route, { data: { tenant } })
    }
    if (path === '/api/platform/v1/tenants/t1/domains' && method === 'GET') {
      return json(route, {
        data: {
          domains: [
            {
              id: 'd1',
              tenant_id: 't1',
              host: 'acme.apps.test',
              kind: 'subdomain',
              verified_at: new Date(0).toISOString(),
              tls_status: tlsStatus,
            },
          ],
        },
      })
    }
    if (path === '/api/platform/v1/tenants/t1/domains/d1/probe-tls' && method === 'POST') {
      tlsStatus = 'issued'
      return json(route, {
        data: { domain_id: 'd1', host: 'acme.apps.test', tls_status: 'issued', error: '' },
      })
    }
    if (path === '/api/platform/v1/tenants/t1/bill/reconcile' && method === 'GET') {
      return json(route, {
        data: {
          month: '2026-11',
          drift: false,
          rollup: { tokens_in: 10, tokens_out: 5, cost_cents: 12, runs: 1 },
          tenant_db: { tokens_in: 10, tokens_out: 5, cost_cents: 12, runs: 1 },
        },
      })
    }
    if (path === '/api/platform/v1/tenants/t1/invoice' && method === 'GET') {
      return json(route, {
        data: {
          number: 'INV-ACME-202611',
          month: '2026-11',
          plan_id: 'standard',
          base_cents: 5000,
          usage_cents: 12,
          total_cents: 5012,
          payment: { provider: 'external', status: 'pending' },
          lines: [],
        },
      })
    }
    if (path === '/api/platform/v1/tenants/t1/provider-keys' && method === 'GET') {
      return json(route, { data: { keys: [] } })
    }
    return json(route, { error: { code: 'not_found', message: path } }, 404)
  })
}

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    window.localStorage.setItem('chiron.locale', 'zh')
    window.sessionStorage.setItem('chiron_platform_token', 'e2e-token')
  })
  await mockConsoleApi(page)
})

test('login requires TOTP and reaches tenant list', async ({ page }) => {
  await page.goto('/login')
  await page.locator('input[autocomplete="username"]').fill('ops@acme.test')
  await page.locator('input[type="password"]').fill('CorrectHorse1!')
  await page.locator('button[type="submit"]').click()

  const otp = page.locator('input[autocomplete="one-time-code"]')
  await expect(otp).toBeVisible()
  await otp.fill('123456')
  await page.locator('button[type="submit"]').click()
  await expect(page).toHaveURL(/\/tenants$/)
  await expect(page.getByRole('heading', { name: '租户' })).toBeVisible()
})

test('creates a tenant and opens its detail page', async ({ page }) => {
  await page.goto('/tenants/new')
  await page.locator('input').nth(0).fill('acme')
  await page.locator('input').nth(1).fill('Acme Inc.')
  await page.locator('input').nth(2).fill('default')
  await page.locator('input').nth(3).fill('owner@acme.test')
  await page.locator('button[type="submit"]').click()
  await expect(page).toHaveURL(/\/tenants\/t1$/)
  await expect(page.getByRole('heading', { name: 'Acme Inc.' })).toBeVisible()
  await page.getByRole('button', { name: '发票' }).click()
  await expect(page.getByText('INV-ACME-202611', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: '对账' }).click()
  await expect(page.getByText('"drift": false', { exact: false })).toBeVisible()
})

test('probes TLS and refreshes the domain status', async ({ page }) => {
  await page.goto('/tenants/t1')
  await expect(page.getByText('acme.apps.test')).toBeVisible()
  await page.getByRole('button', { name: '检测 TLS' }).click()
  await expect(page.getByText('issued')).toBeVisible()
})

test('enables TOTP and shows one-time recovery codes', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('button', { name: '启用二次验证' }).click()
  await expect(page.getByText('JBSWY3DPEHPK3PXP', { exact: true })).toBeVisible()
  await page.locator('input[autocomplete="one-time-code"]').fill('123456')
  await page.getByRole('button', { name: '确认启用' }).click()
  await expect(page.getByText('AAAAA-BBBBB')).toBeVisible()
})

test('saves Alipay integration settings', async ({ page }) => {
  await page.goto('/integrations')
  await page.locator('input').nth(0).fill('2021000000000000')
  await page.locator('input').nth(1).fill('https://platform.example.com/api/platform/v1/billing/alipay/notify')
  await page.locator('input').nth(2).fill('https://platform.example.com/return')
  await page.locator('textarea').nth(0).fill('-----BEGIN PRIVATE KEY-----\ntest\n-----END PRIVATE KEY-----')
  await page.locator('textarea').nth(1).fill('-----BEGIN PUBLIC KEY-----\ntest\n-----END PUBLIC KEY-----')
  await page.getByRole('button', { name: '保存配置' }).click()
  await expect(page.getByText('配置已保存')).toBeVisible()
  await expect(page.getByText('已配置')).toBeVisible()
})

test('saves alert and rate settings', async ({ page }) => {
  await page.goto('/settings')
  await page.locator('input').nth(0).fill('https://hooks.example.com/chiron')
  await page.locator('form').nth(0).getByRole('button', { name: '保存' }).click()
  await expect(page.getByText('告警配置已保存')).toBeVisible()
  await page.locator('form').nth(1).getByRole('button', { name: '保存' }).click()
  await expect(page.getByText('限流配置已保存')).toBeVisible()
})

test('saves media and provider fallback settings', async ({ page }) => {
  await page.goto('/settings')
  await page.locator('form').nth(2).locator('select').selectOption('webdav')
  await page.locator('form').nth(2).locator('input').nth(0).fill('https://dav.example.com')
  await page.locator('form').nth(2).getByRole('button', { name: '保存' }).click()
  await expect(page.getByText('媒体存储配置已保存')).toBeVisible()

  await page.locator('form').nth(3).locator('select').selectOption('openai')
  await page.locator('form').nth(3).locator('input[type="password"]').fill('sk-platform-test')
  await page.locator('form').nth(3).getByRole('button', { name: '保存' }).click()
  await expect(page.getByText('Provider key 已保存')).toBeVisible()
})
