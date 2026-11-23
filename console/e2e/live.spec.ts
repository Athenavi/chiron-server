import { expect, test } from '@playwright/test'

const LIVE_URL = process.env.LIVE_URL

test.skip(!LIVE_URL, 'set LIVE_URL to run against a real chiron-server')

test('live console login and tenant creation', async ({ page }) => {
  const email = process.env.LIVE_EMAIL || 'admin@chiron.local'
  const password = process.env.LIVE_PASSWORD || ''
  const slug = `it-${Date.now().toString(36)}`
  if (!password) throw new Error('LIVE_PASSWORD is required')

  await page.goto(`${LIVE_URL}/login`)
  await page.locator('input[autocomplete="username"]').fill(email)
  await page.locator('input[type="password"]').fill(password)
  await page.locator('button[type="submit"]').click()
  await expect(page).toHaveURL(/\/tenants$/)
  await expect(page.getByRole('heading', { name: '租户' })).toBeVisible()

  await page.getByRole('link', { name: '新建租户' }).click()
  await expect(page).toHaveURL(/\/tenants\/new$/)
  await page.locator('input').nth(0).fill(slug)
  await page.locator('input').nth(1).fill('Integration Tenant')
  await page.locator('input').nth(2).fill('default')
  await page.locator('input').nth(3).fill(`owner-${slug}@example.com`)
  await page.locator('button[type="submit"]').click()
  await expect(page).toHaveURL(/\/tenants\/[0-9a-f-]+$/)
  await expect(page.getByRole('heading', { name: 'Integration Tenant' })).toBeVisible()

  await page.getByRole('button', { name: '发票' }).click()
  await expect(page.getByText('"number": "INV-', { exact: false })).toBeVisible()

  await page.goto(`${LIVE_URL}/settings`)
  await page.locator('form').nth(0).locator('input').nth(0).fill('https://hooks.example.com/chiron')
  await page.locator('form').nth(0).getByRole('button', { name: '保存' }).click()
  await expect(page.getByText('告警配置已保存')).toBeVisible()
})
