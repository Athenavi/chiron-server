import type { Metadata, Viewport } from 'next'
import { Toast } from '@heroui/react'
import './globals.css'
import { AuthProvider } from '@/components/auth/AuthProvider'
import { I18nProvider } from '@/lib/i18n'
import { ThemeProvider } from '@/components/theme/ThemeProvider'
import { THEME_INIT_SCRIPT } from '@/components/theme/theme-config'

export const metadata: Metadata = {
  title: {
    default: 'Chiron',
    template: '%s · Chiron',
  },
  description:
    'Chiron — 面向团队的多租户 AI Agent 工作台 / A multi-tenant AI Agent workspace for teams',
  applicationName: 'Chiron',
}

export const viewport: Viewport = {
  width: 'device-width',
  initialScale: 1,
  viewportFit: 'cover',
  themeColor: [
    { media: '(prefers-color-scheme: light)', color: '#f7f7f8' },
    { media: '(prefers-color-scheme: dark)', color: '#101217' },
  ],
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN">
      <body>
        {/*
          首屏主题脚本：读 localStorage 并写 <html data-theme>，先于页面内容执行，
          避免深色主题用户看到一帧浅色。脚本自带 try/catch，不阻塞渲染。
        */}
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT_SCRIPT }} />
        <I18nProvider>
          <ThemeProvider>
            {/* HeroUI 全局提示区：页面里直接用 `toast.success(...)` 等全局方法即可 */}
            <Toast.Provider />
            <AuthProvider>{children}</AuthProvider>
          </ThemeProvider>
        </I18nProvider>
      </body>
    </html>
  )
}
