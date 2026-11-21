'use client'

/**
 * 思考过程面板：默认折叠，流式时头部显示 spinner 与字数。
 * 内容只做纯文本展示（保留换行），不跑 Markdown，避免长推理反复解析。
 */

import { memo, useId, useState } from 'react'
import { Spinner } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { ChevronDownIcon, SparkIcon } from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

export interface ReasoningPanelProps {
  text: string
  streaming?: boolean
}

export const ReasoningPanel = memo(function ReasoningPanel({ text, streaming }: ReasoningPanelProps) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const panelId = useId()
  if (!text.trim()) return null

  return (
    <div className="rounded-lg border border-dashed border-border bg-surface-secondary/50">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-controls={panelId}
        className="flex w-full items-center gap-2 px-2.5 py-2.5 text-left md:py-1.5"
      >
        <span className="shrink-0 text-muted">
          {streaming ? <Spinner size="sm" color="current" /> : <SparkIcon size={13} />}
        </span>
        <span className="text-[12px] font-medium text-muted">
          {streaming ? t('chat.reasoning.thinking') : t('chat.reasoning.title')}
        </span>
        <span className="tabular text-[11px] text-subtle">
          {t('chat.reasoning.chars', { n: text.length })}
        </span>
        <span className="flex-1" />
        <ChevronDownIcon
          size={13}
          className={cn('shrink-0 text-muted transition-transform', open && 'rotate-180')}
        />
      </button>
      {open ? (
        <div
          id={panelId}
          className="max-h-64 overflow-y-auto border-t border-dashed border-border px-3 py-2 text-[12.5px] leading-relaxed whitespace-pre-wrap break-words text-muted"
        >
          {text}
        </div>
      ) : null}
    </div>
  )
})
