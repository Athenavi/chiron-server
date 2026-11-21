'use client'

/**
 * 工具调用卡片：紧凑地嵌在 assistant 消息流里。
 *
 * - 头部显示工具名 + 状态 chip（运行中 / 已完成 / 失败 / 已拒绝 / 等待中）；
 * - 参数与结果默认折叠，失败/拒绝时自动展开，方便直接看到错误；
 * - 长 JSON 内部滚动，不撑高气泡。
 */

import { memo, useEffect, useId, useState, type ReactNode } from 'react'
import { Chip, Spinner } from '@heroui/react'
import { AlertIcon, ChevronDownIcon, WrenchIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import type { ToolCallView, ToolStatus } from './types'

const CHIP_COLOR: Record<ToolStatus, 'default' | 'accent' | 'success' | 'danger' | 'warning'> = {
  pending: 'default',
  running: 'accent',
  success: 'success',
  error: 'danger',
  denied: 'warning',
}

function formatValue(value: unknown): string {
  if (value === undefined) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

function DetailBlock({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-subtle">{label}</p>
      {children}
    </div>
  )
}

function CodeBox({ code, tone }: { code: string; tone: 'default' | 'danger' }) {
  return (
    <pre
      className={cn(
        'max-h-40 overflow-auto rounded-md border px-2.5 py-2 font-mono text-[12px] leading-relaxed',
        tone === 'danger'
          ? 'border-danger/30 bg-danger/5 text-danger'
          : 'border-border bg-surface text-foreground',
      )}
    >
      <code>{code}</code>
    </pre>
  )
}

export interface ToolCallCardProps {
  tool: ToolCallView
}

export const ToolCallCard = memo(function ToolCallCard({ tool }: ToolCallCardProps) {
  const { t } = useI18n()
  const detailsId = useId()
  const failed = tool.status === 'error' || tool.status === 'denied'
  const [open, setOpen] = useState(failed)
  const label = t(`chat.tool.status.${tool.status}`)
  const hasArgs = tool.args !== undefined && tool.args !== null
  const hasResult = tool.result !== undefined && tool.result !== null
  const hasDetail = hasArgs || hasResult || Boolean(tool.error)

  useEffect(() => {
    if (failed) setOpen(true)
  }, [failed])

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-surface-secondary/60">
      <button
        type="button"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-controls={hasDetail ? detailsId : undefined}
        className="flex w-full items-center gap-2 px-2.5 py-2.5 text-left transition-colors hover:bg-surface-hover md:py-1.5"
      >
        <span className="shrink-0 text-muted">
          {tool.status === 'running' ? <Spinner size="sm" color="current" /> : <WrenchIcon size={13} />}
        </span>
        <span
          className="min-w-0 flex-1 truncate font-mono text-[12px] text-foreground"
          title={tool.name}
        >
          {tool.name}
        </span>
        <span className="sr-only">{t('chat.tool.statusAria', { status: label })}</span>
        <Chip size="sm" variant="soft" color={CHIP_COLOR[tool.status]} className="shrink-0">
          {label}
        </Chip>
        {hasDetail ? (
          <ChevronDownIcon
            size={13}
            className={cn('shrink-0 text-muted transition-transform', open && 'rotate-180')}
          />
        ) : null}
      </button>

      {open && hasDetail ? (
        <div
          id={detailsId}
          className="flex flex-col gap-2 border-t border-border px-2.5 py-2"
        >
          {hasArgs ? (
            <DetailBlock label={t('chat.tool.args')}>
              <CodeBox code={formatValue(tool.args)} tone="default" />
            </DetailBlock>
          ) : null}
          {hasResult ? (
            <DetailBlock label={t('chat.tool.result')}>
              <CodeBox code={formatValue(tool.result)} tone="default" />
            </DetailBlock>
          ) : null}
          {tool.error ? (
            <DetailBlock label={t('chat.tool.error')}>
              <div className="flex items-start gap-1.5 rounded-md border border-danger/30 bg-danger/5 px-2.5 py-2 text-[12px] text-danger">
                <AlertIcon size={13} className="mt-0.5 shrink-0" />
                <span className="break-words">{tool.error}</span>
              </div>
            </DetailBlock>
          ) : null}
        </div>
      ) : null}
    </div>
  )
})
