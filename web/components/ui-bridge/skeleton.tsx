/**
 * 兼容层：旧 Skeleton 调用点（支持 `lines`）→ HeroUI Skeleton。
 * 只做结构拼装，不自定义颜色/动画（全部来自 HeroUI 变量）。
 */

import type { CSSProperties } from 'react'
import { Skeleton as HeroSkeleton } from '@heroui/react'
import { cn } from '@/lib/utils'

export interface SkeletonProps {
  className?: string
  style?: CSSProperties
  /** 多行文本骨架的行数。 */
  lines?: number
}

export function Skeleton({ className, style, lines }: SkeletonProps) {
  if (lines && lines > 1) {
    return (
      <div className={cn('flex flex-col gap-2', className)} aria-hidden="true" style={style}>
        {Array.from({ length: lines }, (_, index) => (
          <HeroSkeleton
            key={index}
            className="h-3 rounded-sm"
            style={{ width: index === lines - 1 ? '62%' : '100%' }}
          />
        ))}
      </div>
    )
  }

  return (
    <HeroSkeleton className={cn('h-3 w-full rounded-sm', className)} style={style} aria-hidden="true" />
  )
}
