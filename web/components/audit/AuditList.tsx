'use client'

/**
 * 审计列表：桌面端语义表格 + 移动端卡片。
 *
 * - 动作统一用 Chip（颜色由 `audit-meta` 的规则决定）；
 * - `user_id` 为空时显示「系统/匿名」；`target` 作为副行小字；
 * - 详情超过阈值时折叠为一行，可逐行展开 / 收起；
 * - 骨架屏保持与真实行相同的节奏；
 * - 表头 / caption / aria 与按钮文案走 `audit.list.*` i18n，时间按 locale 格式化。
 */

import { useState } from 'react'
import type { AuditEntry } from '@/lib/types'
import { Skeleton } from '@/components/ui-bridge/skeleton'
import { useI18n } from '@/lib/i18n'
import {
  AuditActionChip,
  auditAbsolute,
  auditDetailIsLong,
  auditTimestamp,
  auditUserLabel,
} from './audit-meta'

export function AuditList({ entries }: { entries: AuditEntry[] }) {
  const { t, locale } = useI18n()
  const [expanded, setExpanded] = useState<Set<number>>(() => new Set<number>())

  function toggleDetail(id: number) {
    setExpanded((previous) => {
      const next = new Set(previous)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <>
      {/* 桌面：语义表格 */}
      <div className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
        <table className="w-full border-collapse text-left text-sm" aria-label={t('audit.list.label')}>
          <caption className="sr-only">{t('audit.list.caption')}</caption>
          <thead>
            <tr className="border-b border-border bg-surface-secondary/60 text-[11px] tracking-wide text-subtle">
              <th scope="col" className="whitespace-nowrap px-4 py-2.5 font-medium">
                {t('audit.list.time')}
              </th>
              <th scope="col" className="px-3 py-2.5 font-medium">
                {t('audit.list.action')}
              </th>
              <th scope="col" className="px-3 py-2.5 font-medium">
                {t('audit.list.user')}
              </th>
              <th scope="col" className="px-3 py-2.5 font-medium">
                {t('audit.list.detail')}
              </th>
              <th scope="col" className="whitespace-nowrap px-4 py-2.5 font-medium">
                {t('audit.list.ip')}
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {entries.map((entry) => {
              const isExpanded = expanded.has(entry.id)
              const long = auditDetailIsLong(entry.detail)
              return (
                <tr key={entry.id} className="align-top transition-colors hover:bg-surface-hover/60">
                  <td className="whitespace-nowrap px-4 py-3 text-[12px] text-muted tabular">
                    <span title={auditAbsolute(entry.created_at, locale)}>
                      {auditTimestamp(entry.created_at, locale)}
                    </span>
                  </td>
                  <td className="px-3 py-3">
                    <AuditActionChip action={entry.action} />
                  </td>
                  <td className="px-3 py-3">
                    <span className="block max-w-[18ch] truncate text-[13px] text-foreground">
                      {auditUserLabel(entry.user_id, t)}
                    </span>
                    {entry.target ? (
                      <span
                        className="mt-0.5 block max-w-[20ch] truncate text-[11px] text-muted"
                        title={entry.target}
                      >
                        {entry.target}
                      </span>
                    ) : null}
                  </td>
                  <td className="px-3 py-3">
                    <div className="max-w-[36ch]">
                      <p
                        className={
                          isExpanded
                            ? 'break-words text-[12px] leading-5 text-muted'
                            : 'line-clamp-1 text-[12px] leading-5 text-muted'
                        }
                      >
                        {entry.detail?.trim() || '—'}
                      </p>
                      {long ? (
                        <button
                          type="button"
                          onClick={() => toggleDetail(entry.id)}
                          aria-expanded={isExpanded}
                          className="mt-0.5 rounded text-[11px] text-accent transition-colors hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                        >
                          {isExpanded ? t('audit.list.collapse') : t('audit.list.expand')}
                        </button>
                      ) : null}
                    </div>
                  </td>
                  <td className="whitespace-nowrap px-4 py-3 font-mono text-[11px] text-subtle">
                    {entry.ip || '—'}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      {/* 移动端：卡片 */}
      <ul className="flex flex-col gap-2 md:hidden" aria-label={t('audit.list.label')}>
        {entries.map((entry) => {
          const isExpanded = expanded.has(entry.id)
          const long = auditDetailIsLong(entry.detail)
          return (
            <li
              key={entry.id}
              className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs"
            >
              <div className="flex items-start justify-between gap-2">
                <AuditActionChip action={entry.action} />
                <span className="shrink-0 text-[11px] text-subtle tabular">
                  {auditTimestamp(entry.created_at, locale)}
                </span>
              </div>
              <div className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted">
                <span className="truncate">{auditUserLabel(entry.user_id, t)}</span>
                {entry.target ? (
                  <>
                    <span aria-hidden="true">·</span>
                    <span className="truncate">{entry.target}</span>
                  </>
                ) : null}
                <span aria-hidden="true">·</span>
                <span className="font-mono text-subtle">{entry.ip || '—'}</span>
              </div>
              <div className="mt-1.5">
                <p
                  className={
                    isExpanded
                      ? 'break-words text-[12px] leading-5 text-muted'
                      : 'line-clamp-2 text-[12px] leading-5 text-muted'
                  }
                >
                  {entry.detail?.trim() || '—'}
                </p>
                {long ? (
                  <button
                    type="button"
                    onClick={() => toggleDetail(entry.id)}
                    aria-expanded={isExpanded}
                    className="mt-0.5 rounded text-[11px] text-accent transition-colors hover:underline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                  >
                    {isExpanded ? t('audit.list.collapse') : t('audit.list.expand')}
                  </button>
                ) : null}
              </div>
            </li>
          )
        })}
      </ul>
    </>
  )
}

/** 加载骨架（桌面表格行 + 移动端卡片）。 */
export function AuditListSkeleton() {
  const { t } = useI18n()
  const rows = [0, 1, 2, 3, 4]

  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{t('audit.skeleton')}</span>

      <div className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
        <div className="flex items-center gap-4 border-b border-border bg-surface-secondary/60 px-4 py-3">
          <Skeleton className="h-2.5 w-16" />
          <Skeleton className="h-2.5 w-20" />
          <Skeleton className="h-2.5 w-24" />
          <Skeleton className="h-2.5 w-12" />
        </div>
        <ul className="divide-y divide-border">
          {rows.map((row) => (
            <li key={row} className="flex items-center gap-4 px-4 py-3.5">
              <Skeleton className="h-3 w-20 shrink-0" />
              <Skeleton className="h-5 w-16 shrink-0 rounded-full" />
              <Skeleton className="h-3" style={{ width: `${28 + row * 6}%` }} />
              <Skeleton className="ms-auto h-3 w-24 shrink-0" />
            </li>
          ))}
        </ul>
      </div>

      <ul className="flex flex-col gap-2 md:hidden" aria-hidden="true">
        {rows.map((row) => (
          <li key={row} className="rounded-xl border border-border bg-surface p-3 shadow-xs">
            <div className="flex items-center justify-between gap-2">
              <Skeleton className="h-5 w-16 rounded-full" />
              <Skeleton className="h-3 w-20" />
            </div>
            <Skeleton className="mt-2.5 h-3" style={{ width: `${44 + row * 8}%` }} />
            <Skeleton className="mt-2 h-2.5 w-32" />
          </li>
        ))}
      </ul>
    </div>
  )
}
