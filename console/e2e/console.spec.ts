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
