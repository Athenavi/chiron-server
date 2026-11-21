import type { Metadata } from 'next'
import './globals.css'
import { ConsoleNav, LocaleProvider } from '@/lib/i18n'
import { LOCALE_BOOTSTRAP_SCRIPT } from '@/lib/messages'

export const metadata: Metadata = { title: 'Chiron Platform', description: 'Multi-tenant AI agent control plane' }

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN" suppressHydrationWarning>
      <body>
        <script dangerouslySetInnerHTML={{ __html: LOCALE_BOOTSTRAP_SCRIPT }} />
        <LocaleProvider>
          <ConsoleNav />
          <main>{children}</main>
        </LocaleProvider>
      </body>
    </html>
  )
}
