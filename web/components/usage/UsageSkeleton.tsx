'use client'

/**
 * 用量页加载骨架：四张概览卡片 + 一块明细表，保持与真实布局相同的节奏。
 */

import { Skeleton } from '@/components/ui-bridge/skeleton'
import { useI18n } from '@/lib/i18n'

export function UsageSkeleton() {
  const { t } = useI18n()

  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{t('usage.skeleton')}</span>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        {[0, 1, 2, 3].map((index) => (
          <div
            key={index}
            className="rounded-xl border border-border bg-surface p-4 shadow-xs"
            aria-hidden="true"
          >
            <Skeleton className="h-2.5 w-16" />
            <Skeleton className="mt-3 h-6" style={{ width: `${52 + index * 6}%` }} />
            <Skeleton className="mt-2.5 h-2.5 w-24" />
          </div>
        ))}
      </div>

      <div className="flex flex-wrap items-center gap-2 pt-1" aria-hidden="true">
        <Skeleton className="h-8 w-20 rounded-full" />
        <Skeleton className="h-8 w-20 rounded-full" />
        <Skeleton className="h-8 w-20 rounded-full" />
      </div>

      <div className="overflow-hidden rounded-xl border border-border bg-surface shadow-xs" aria-hidden="true">
        <div className="flex items-center gap-4 border-b border-border bg-surface-secondary/60 px-4 py-3">
          <Skeleton className="h-2.5 w-24" />
          <Skeleton className="ms-auto h-2.5 w-12" />
          <Skeleton className="h-2.5 w-12" />
          <Skeleton className="h-2.5 w-16" />
        </div>
        <ul className="divide-y divide-border">
          {[0, 1, 2, 3].map((row) => (
            <li key={row} className="flex items-center gap-4 px-4 py-3.5">
              <Skeleton className="h-3" style={{ width: `${30 + row * 8}%` }} />
              <Skeleton className="ms-auto h-3 w-10" />
              <Skeleton className="h-3 w-10" />
              <Skeleton className="h-3 w-16" />
            </li>
          ))}
        </ul>
      </div>
    </div>
  )
}
