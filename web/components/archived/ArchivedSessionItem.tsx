'use client'

/**
 * 归档会话卡片。
 *
 * 只展示标题、模型与时间信息；父会话标记不影响归档列表的使用，故不渲染。
 * 恢复 / 删除的 pending 状态由页面下发，卡片自身不持有异步状态。
 * 文案走 `archived.*` i18n，相对 / 绝对时间按 locale 用 `Intl` 生成。
 */

import { Button } from '@heroui/react'
import { ArchiveIcon, RefreshIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n, type Locale } from '@/lib/i18n'
import type { ChatSession } from '@/lib/types'

export interface ArchivedSessionItemProps {
  session: ChatSession
  /** 该会话正在恢复。 */
  isRestoring?: boolean
  /** 有删除操作在排队 / 进行中，禁用本项所有按钮。 */
  isBusy?: boolean
  onRestore: (session: ChatSession) => void
  onDelete: (session: ChatSession) => void
}

function intlTag(locale: Locale): string {
  return locale === 'en' ? 'en-US' : 'zh-CN'
}

/** 绝对时间：本地时区 `MM-DD HH:mm`。 */
function formatStamp(value: string, locale: Locale): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return new Intl.DateTimeFormat(intlTag(locale), {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(date)
}

/**
 * 相对时间：刚刚 / N 分钟前 / N 小时前 / 昨天 / MM-DD。
 *
 * 中文文案（分钟 / 小时 / 昨天）交给 `Intl.RelativeTimeFormat`，避免为每种语言手写模板。
 */
function formatRelativeStamp(
  value: string,
  locale: Locale,
  justNow: string,
): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  const diff = Date.now() - date.getTime()
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour
  if (diff < minute) return justNow
  const relative = new Intl.RelativeTimeFormat(intlTag(locale), { numeric: 'auto' })
  if (diff < hour) return relative.format(-Math.round(diff / minute), 'minute')
  if (diff < day) return relative.format(-Math.round(diff / hour), 'hour')
  if (diff < 2 * day) return relative.format(-1, 'day')
  return new Intl.DateTimeFormat(intlTag(locale), { month: '2-digit', day: '2-digit' }).format(date)
}

export function ArchivedSessionItem({
  session,
  isRestoring = false,
  isBusy = false,
  onRestore,
  onDelete,
}: ArchivedSessionItemProps) {
  const { t, locale } = useI18n()
  const title = session.title?.trim() || t('archived.untitled')

  return (
    <article className="min-w-0 rounded-xl border border-border bg-surface p-3.5 shadow-xs sm:p-4">
      <div className="flex flex-wrap items-start justify-between gap-x-3 gap-y-3">
        <div className="min-w-0 flex-1">
          <h2 className="truncate text-sm font-semibold text-foreground">{title}</h2>
          <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted">
            <span className="inline-flex max-w-full items-center gap-1 rounded-full bg-surface-secondary px-2 py-0.5">
              <ArchiveIcon size={11} aria-hidden="true" />
              <span className="truncate font-mono">
                {session.model ?? t('archived.defaultModel')}
              </span>
            </span>
            <span className="tabular" title={formatStamp(session.updated_at, locale)}>
              {t('archived.updatedAt', {
                relative: formatRelativeStamp(session.updated_at, locale, t('archived.relative.justNow')),
              })}
            </span>
            {session.archived_at ? (
              <span className="tabular">
                {t('archived.archivedAt', { time: formatStamp(session.archived_at, locale) })}
              </span>
            ) : null}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            className="h-8 gap-1.5"
            isPending={isRestoring}
            isDisabled={isBusy}
            onPress={() => onRestore(session)}
          >
            {isRestoring ? null : <RefreshIcon size={13} />}
            {t('archived.restore')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="h-8 gap-1.5 text-danger"
            isDisabled={isBusy || isRestoring}
            onPress={() => onDelete(session)}
          >
            <TrashIcon size={13} />
            {t('archived.delete')}
          </Button>
        </div>
      </div>
    </article>
  )
}
