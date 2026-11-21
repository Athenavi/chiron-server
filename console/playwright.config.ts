import { defineConfig, devices } from '@playwright/test'

const port = 3100

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? 'github' : 'list',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: 'retain-on-failure',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: process.env.CI
      ? `npm run start -- -p ${port}`
      : `npm run dev -- -p ${port}`,
    url: `http://127.0.0.1:${port}/login`,
    timeout: 120_000,
    reuseExistingServer: !process.env.CI,
  },
})
