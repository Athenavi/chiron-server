'use client'

/** 文件网格骨架：与真实网格同列宽，避免加载完成时布局跳动。 */

import { Skeleton } from '@/components/ui-bridge/skeleton'

const GRID_CLASS = 'grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4'

export function MediaGridSkeleton({ count = 8 }: { count?: number }) {
  return (
    <div className={GRID_CLASS} aria-hidden="true">
      {Array.from({ length: count }, (_, index) => (
        <div
          key={index}
          className="overflow-hidden rounded-xl border border-border bg-surface shadow-xs"
        >
          <Skeleton className="aspect-[4/3] w-full rounded-none" />
          <div className="flex flex-col gap-2 p-3">
            <Skeleton className="h-3.5 w-4/5 rounded-sm" />
            <Skeleton className="h-2.5 w-2/5 rounded-sm" />
            <Skeleton className="h-2.5 w-3/5 rounded-sm" />
            <div className="mt-1 flex gap-1.5">
              <Skeleton className="h-10 w-10 rounded-lg" />
              <Skeleton className="h-10 flex-1 rounded-lg" />
              <Skeleton className="h-10 w-10 rounded-lg" />
            </div>
          </div>
        </div>
      ))}
    </div>
  )
}
