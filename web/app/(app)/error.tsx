'use client'

/**
 * `(app)` 段错误边界：页面渲染期抛错时兜底，保留「重试 / 返回会话」两个出口。
 *
 * 只处理渲染异常；接口层的 401/403/404/429 已在各页面内部转成可读状态。
 */

import { useEffect } from 'react'
import Link from 'next/link'
import { Button } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { AlertIcon, ChevronLeftIcon, RefreshIcon } from '@/components/ui-bridge/icons'

export default function AppError({
  error,
  reset,
}: {
  error: Error & { digest?: string }
  reset: () => void
}) {
  const { t } = useI18n()

  useEffect(() => {
    // 浏览器控制台留一份可追溯的堆栈；不额外引入上报依赖
    console.error('App render error:', error)
  }, [error])

  return (
    <div className="flex min-h-[60dvh] items-center justify-center px-4 py-10">
      <div
        role="alert"
        className="flex w-full max-w-md flex-col items-center gap-3 rounded-xl border border-border bg-surface px-6 py-8 text-center shadow-xs"
      >
        <span className="flex h-11 w-11 items-center justify-center rounded-full border border-border bg-surface-secondary text-danger">
          <AlertIcon size={20} aria-hidden="true" />
        </span>
        <h1 className="text-base font-semibold text-foreground">{t('common.pageErrorTitle')}</h1>
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
  )
}
