'use client'

/**
 * 聊天页错误边界：会话页渲染期抛错时的兜底，视觉与聊天页的 ErrorState 保持一致。
 */

import { useEffect } from 'react'
import Link from 'next/link'
import { Button } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { AlertIcon, ChevronLeftIcon, RefreshIcon } from '@/components/ui-bridge/icons'

export default function ChatError({
  error,
  reset,
}: {
  error: Error & { digest?: string }
  reset: () => void
}) {
  const { t } = useI18n()

  useEffect(() => {
    console.error('Chat render error:', error)
  }, [error])

  return (
    <div className="flex h-dvh min-h-0 flex-col bg-background">
      <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background px-4">
        <Link
          href="/sessions"
          aria-label={t('chat.page.backToSessions')}
          className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted transition-colors hover:bg-surface-hover hover:text-foreground"
        >
          <ChevronLeftIcon size={16} aria-hidden="true" />
        </Link>
        <span className="text-sm font-semibold text-foreground">{t('chat.page.sessionLabel')}</span>
      </header>
      <div className="flex min-h-0 flex-1 items-center justify-center px-4">
        <div
          role="alert"
          className="flex w-full max-w-md flex-col items-center gap-3 rounded-xl border border-border bg-surface px-6 py-8 text-center shadow-xs"
        >
          <span className="flex h-11 w-11 items-center justify-center rounded-full border border-border bg-surface-secondary text-danger">
            <AlertIcon size={20} aria-hidden="true" />
          </span>
          <h1 className="text-base font-semibold text-foreground">
            {t('chat.page.errorCrashTitle')}
          </h1>
          <p className="text-sm text-muted">{error.message || t('common.pageErrorBody')}</p>
          {error.digest ? (
            <p className="font-mono text-[11px] text-subtle">digest: {error.digest}</p>
          ) : null}
          <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
            <Button variant="secondary" onPress={reset} className="min-h-10">
              <RefreshIcon size={14} aria-hidden="true" />
              {t('common.retry')}
            </Button>
            <Link
              href="/sessions"
              className="inline-flex min-h-10 items-center gap-1.5 rounded-md border border-border px-3 text-sm text-foreground transition-colors hover:bg-surface-hover"
            >
              <ChevronLeftIcon size={14} aria-hidden="true" />
              {t('common.backToSessions')}
            </Link>
          </div>
        </div>
      </div>
    </div>
  )
}
