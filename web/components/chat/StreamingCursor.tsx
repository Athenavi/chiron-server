'use client'

/**
 * 流式光标：一个闪烁块 + 供屏幕阅读器播报的状态文本。
 * 视觉部分由 `app/globals.css` 的 `.stream-caret` 提供，避免在这里重复动画定义。
 */

import { useI18n } from '@/lib/i18n'

export function StreamingCursor({ label }: { label?: string }) {
  const { t } = useI18n()
  return (
    <span className="inline-flex items-baseline">
      <span className="stream-caret" aria-hidden="true" />
      <span className="sr-only">{label ?? t('chat.streaming.label')}</span>
    </span>
  )
}

export default StreamingCursor
