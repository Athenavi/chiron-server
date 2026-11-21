'use client'

/**
 * 归档会话列表骨架：首屏 / 刷新时占位，结构与卡片保持一致，避免布局跳动。
 */

import { Skeleton } from '@heroui/react'
import { useI18n } from '@/lib/i18n'

export function ArchivedListSkeleton({ rows = 3 }: { rows?: number }) {
  const { t } = useI18n()

  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{t('archived.skeleton')}</span>
      {Array.from({ length: rows }, (_, index) => (
        <div
          key={index}
          className="rounded-xl border border-border bg-surface p-3.5 shadow-xs sm:p-4"
          aria-hidden="true"
        >
          <div className="flex items-center gap-3">
            <div className="min-w-0 flex-1">
              <Skeleton className="h-3.5 w-2/5" />
              <div className="mt-2.5 flex items-center gap-2">
                <Skeleton className="h-4 w-28 rounded-full" />
                <Skeleton className="h-2.5 w-20" />
                <Skeleton className="h-2.5 w-24" />
              </div>
            </div>
            <Skeleton className="h-8 w-16 shrink-0 rounded-md" />
            <Skeleton className="h-8 w-16 shrink-0 rounded-md" />
          </div>
          <Skeleton className="mt-3 h-2 w-1/4" />
        </div>
      ))}
    </div>
  )
}
