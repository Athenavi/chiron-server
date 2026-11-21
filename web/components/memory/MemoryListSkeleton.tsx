'use client'

/**
 * 记忆列表骨架屏：切换槽位筛选 / 首次加载时占位，结构与卡片保持一致。
 *
 * `role="status"` + `aria-busy` 让读屏用户知道正在加载；可见骨架对读屏隐藏。
 */

import { Skeleton } from '@heroui/react'
import { useI18n } from '@/lib/i18n'

export function MemoryListSkeleton({ rows = 3 }: { rows?: number }) {
  const { t } = useI18n()

  return (
    <div
      role="status"
      aria-live="polite"
      aria-busy="true"
      className="flex flex-col gap-3"
    >
      <span className="sr-only">{t('memory.skeleton.aria')}</span>
      {Array.from({ length: rows }, (_, index) => (
        <div
          key={index}
          aria-hidden="true"
          className="rounded-xl border border-border bg-surface p-3.5 shadow-xs sm:p-4"
        >
          <div className="flex items-center gap-2">
            <Skeleton className="h-5 w-14 rounded-full" />
            <Skeleton className="h-2.5 w-20" />
            <Skeleton className="h-2.5 w-16" />
            <Skeleton className="ml-auto h-6 w-16 rounded-md" />
          </div>
          <Skeleton className="mt-3 h-3" style={{ width: `${86 - index * 12}%` }} />
          <Skeleton className="mt-2 h-3" style={{ width: `${64 - index * 8}%` }} />
          <Skeleton className="mt-3 h-2.5 w-24" />
        </div>
      ))}
    </div>
  )
}
