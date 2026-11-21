'use client'

/**
 * Provider 卡片网格骨架：首屏 / 刷新时占位，结构与卡片保持一致。
 */

import { Skeleton } from '@heroui/react'
import { useI18n } from '@/lib/i18n'

export function ProviderGridSkeleton({ cards = 4 }: { cards?: number }) {
  const { t } = useI18n()

  return (
    <div
      role="status"
      aria-live="polite"
      aria-busy="true"
      className="mt-4 grid grid-cols-1 gap-3 lg:grid-cols-2"
    >
      <span className="sr-only">{t('models.skeleton')}</span>
      {Array.from({ length: cards }, (_, index) => (
        <div
          key={index}
          className="rounded-xl border border-border bg-surface p-4 shadow-xs"
          aria-hidden="true"
        >
          <div className="flex items-center gap-2">
            <Skeleton className="h-3.5 w-24" />
            <Skeleton className="h-2.5 w-16" />
            <Skeleton className="ml-auto h-5 w-16 rounded-full" />
          </div>
          <div className="mt-3 flex flex-wrap gap-1.5">
            <Skeleton className="h-5 w-20 rounded-full" />
            <Skeleton className="h-5 w-16 rounded-full" />
            <Skeleton className="h-5 w-24 rounded-full" />
          </div>
          <Skeleton className="mt-3.5 h-2.5 w-16" />
          <Skeleton className="mt-2 h-8 w-full rounded-md" />
          <Skeleton className="mt-3.5 h-2.5 w-16" />
          <div className="mt-2 flex flex-wrap gap-1.5">
            <Skeleton className="h-5 w-14 rounded-full" />
            <Skeleton className="h-5 w-12 rounded-full" />
            <Skeleton className="h-5 w-16 rounded-full" />
          </div>
        </div>
      ))}
    </div>
  )
}
