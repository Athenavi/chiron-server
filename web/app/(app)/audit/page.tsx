'use client'

/**
 * 审计页（仅 owner / admin）。
 *
 * - 非 owner/admin 直接展示无权限空态，不发任何请求；
 * - 过滤区：动作关键字、起止日期、每页条数；「应用」重置列表并重新拉取，
 *   「重置」恢复默认条件；
 * - 分页：`next_cursor` 存在时显示「加载更多」，追加结果并按 id 去重；
 * - 列表桌面表格 + 移动端卡片，动作 Chip 带颜色，详情长文本可展开；
 * - 空态 / 骨架 / 错误重试齐备，时间按本地时区展示；
 * - 文案全部走 `audit.*` i18n 命名空间。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import Link from 'next/link'
import { Alert, Button, EmptyState } from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { PageHeader } from '@/components/shell/PageHeader'
import {
  AuditFilters,
  EMPTY_AUDIT_FILTER,
  type AuditFilterDraft,
} from '@/components/audit/AuditFilters'
import { AuditList, AuditListSkeleton } from '@/components/audit/AuditList'
import {
  AlertIcon,
  LockIcon,
  MessageIcon,
  RefreshIcon,
  SearchIcon,
} from '@/components/ui-bridge/icons'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type AuditEntry, type AuditParams } from '@/lib/types'

/** owner / admin 才有权查看租户审计。 */
function isAdminRole(role: string | null | undefined): boolean {
  const key = (role ?? '').trim().toLowerCase()
  return key === 'owner' || key === 'admin'
}

/** `yyyy-mm-dd` → 当天 00:00 的 RFC3339（本地时区）。 */
function toIsoStart(date: string): string | undefined {
  if (!date) return undefined
  const value = new Date(`${date}T00:00:00`)
  return Number.isNaN(value.getTime()) ? undefined : value.toISOString()
}

/** `yyyy-mm-dd` → 当天 23:59:59.999 的 RFC3339（本地时区，含整天）。 */
function toIsoEnd(date: string): string | undefined {
  if (!date) return undefined
  const value = new Date(`${date}T23:59:59.999`)
  return Number.isNaN(value.getTime()) ? undefined : value.toISOString()
}

function buildParams(filters: AuditFilterDraft, cursor?: string | null): AuditParams {
  return {
    action: filters.action.trim() || undefined,
    from: toIsoStart(filters.from),
    to: toIsoEnd(filters.to),
    limit: filters.limit,
    cursor: cursor || undefined,
  }
}

/** 追加分页结果并按 `id` 去重（后端游标可能重叠）。 */
function appendUnique(previous: AuditEntry[], added: AuditEntry[]): AuditEntry[] {
  const seen = new Set(previous.map((entry) => entry.id))
  const merged = [...previous]
  for (const entry of added) {
    if (seen.has(entry.id)) continue
    seen.add(entry.id)
    merged.push(entry)
  }
  return merged
}

export default function AuditPage() {
  const { t, locale } = useI18n()
  const { user } = useAuth()
  const isAdmin = isAdminRole(user?.role)

  const [draft, setDraft] = useState<AuditFilterDraft>(EMPTY_AUDIT_FILTER)
  const [applied, setApplied] = useState<AuditFilterDraft>(EMPTY_AUDIT_FILTER)
  const [entries, setEntries] = useState<AuditEntry[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  /** 保留原始错误对象：错误文案在渲染时按当前 locale 生成，切换语言不会重新拉取。 */
  const [error, setError] = useState<unknown>(null)
  const [moreError, setMoreError] = useState<unknown>(null)

  /** 请求序号：应用新过滤 / 刷新 / 加载更多都以最后一次请求为准。 */
  const requestSeq = useRef(0)

  const load = useCallback(
    async (filters: AuditFilterDraft, options: { silent?: boolean } = {}) => {
      const seq = (requestSeq.current += 1)
      if (options.silent) setRefreshing(true)
      else setLoading(true)
      setLoadingMore(false)
      setError(null)
      setMoreError(null)
      try {
        const result = await api.listAudit(buildParams(filters))
        if (seq !== requestSeq.current) return
        setEntries(appendUnique([], result.audit))
        setNextCursor(result.next_cursor)
      } catch (cause) {
        if (seq !== requestSeq.current) return
        setError(cause)
        setEntries([])
        setNextCursor(null)
      } finally {
        if (seq === requestSeq.current) {
          setLoading(false)
          setRefreshing(false)
        }
      }
    },
    [],
  )

  useEffect(() => {
    if (!isAdmin) return
    void load(applied)
  }, [isAdmin, applied, load])

  const loadMore = useCallback(async () => {
    if (!nextCursor || loading || loadingMore) return
    const seq = (requestSeq.current += 1)
    setLoadingMore(true)
    setMoreError(null)
    try {
      const result = await api.listAudit(buildParams(applied, nextCursor))
      if (seq !== requestSeq.current) return
      setEntries((previous) => appendUnique(previous, result.audit))
      setNextCursor(result.next_cursor)
    } catch (cause) {
      if (seq !== requestSeq.current) return
      setMoreError(cause)
    } finally {
      if (seq === requestSeq.current) setLoadingMore(false)
    }
  }, [applied, loading, loadingMore, nextCursor])

  function applyFilters() {
    setApplied({ ...draft, action: draft.action.trim() })
  }

  function resetFilters() {
    setDraft(EMPTY_AUDIT_FILTER)
    setApplied(EMPTY_AUDIT_FILTER)
  }

  /** 数字与日期按当前 locale 格式化（`zh-CN` / `en-US`）。 */
  const intlTag = locale === 'en' ? 'en-US' : 'zh-CN'
  function formatNumber(value: number): string {
    return new Intl.NumberFormat(intlTag).format(value)
  }

  function formatDate(value: string): string {
    const date = new Date(`${value}T00:00:00`)
    if (Number.isNaN(date.getTime())) return value
    return new Intl.DateTimeFormat(intlTag, {
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
    }).format(date)
  }

  /** 当前过滤条件的可读摘要（「全部记录」或「动作含… · 自… · 至…」）。 */
  function describeFilters(filters: AuditFilterDraft): string {
    const parts: string[] = []
    if (filters.action.trim()) {
      parts.push(t('audit.summary.actionContains', { query: filters.action.trim() }))
    }
    if (filters.from) parts.push(t('audit.summary.from', { date: formatDate(filters.from) }))
    if (filters.to) parts.push(t('audit.summary.to', { date: formatDate(filters.to) }))
    return parts.length > 0 ? parts.join(' · ') : t('audit.summary.all')
  }

  // ── 无权限 ────────────────────────────────────────────────────────────

  if (!isAdmin) {
    return (
      <div>
        <PageHeader title={t('audit.title')} description={t('audit.restrictedDescription')} />
        <div className="content-shell max-w-5xl py-6 md:py-8">
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <LockIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('audit.noAccess.title')}</h2>
            <p className="max-w-md text-xs leading-5 text-muted">{t('audit.noAccess.body')}</p>
            <Link
              href="/sessions"
              className="mt-1 inline-flex h-9 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary-hover"
            >
              <MessageIcon size={15} />
              {t('audit.noAccess.back')}
            </Link>
          </EmptyState>
        </div>
      </div>
    )
  }

  // ── 有权限 ────────────────────────────────────────────────────────────

  const hasFilters = Boolean(applied.action || applied.from || applied.to)
  const busy = loading || refreshing || loadingMore
  const errorMessage = error ? ApiError.from(error, t('audit.error.fallback')).message : null
  const moreErrorMessage = moreError
    ? ApiError.from(moreError, t('audit.error.moreFallback')).message
    : null
  const showEmpty = !loading && !errorMessage && entries.length === 0

  return (
    <div>
      <PageHeader
        title={t('audit.title')}
        description={t('audit.description')}
        actions={
          <Button
            variant="secondary"
            size="sm"
            className="h-9 gap-1.5"
            isPending={refreshing}
            isDisabled={loading}
            onPress={() => void load(applied, { silent: true })}
          >
            {refreshing ? null : <RefreshIcon size={14} />}
            {t('common.refresh')}
          </Button>
        }
      />

      <div className="content-shell max-w-5xl py-6 md:py-8">
        <AuditFilters
          draft={draft}
          onChange={setDraft}
          onApply={applyFilters}
          onReset={resetFilters}
          isBusy={busy}
        />

        {errorMessage ? (
          <Alert status="danger" className="mt-4">
            <Alert.Indicator>
              <AlertIcon aria-hidden="true" className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>{t('audit.error.title')}</Alert.Title>
              <Alert.Description>{errorMessage}</Alert.Description>
            </Alert.Content>
            <Button
              variant="secondary"
              size="sm"
              className="h-8 shrink-0 gap-1.5 self-center"
              onPress={() => void load(applied)}
            >
              <RefreshIcon size={14} />
              {t('common.retry')}
            </Button>
          </Alert>
        ) : null}

        {loading ? (
          <div className="mt-4">
            <AuditListSkeleton />
          </div>
        ) : null}

        {showEmpty ? (
          <EmptyState className="mt-4 flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <SearchIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">
              {hasFilters ? t('audit.empty.titleFiltered') : t('audit.empty.titlePlain')}
            </h2>
            <p className="max-w-sm text-xs leading-5 text-muted">
              {hasFilters ? t('audit.empty.bodyFiltered') : t('audit.empty.bodyPlain')}
            </p>
            {hasFilters ? (
              <Button variant="secondary" className="mt-1 h-9" onPress={resetFilters}>
                {t('audit.empty.reset')}
              </Button>
            ) : (
              <Button variant="ghost" className="mt-1 h-9" onPress={() => void load(applied)}>
                {t('audit.empty.refresh')}
              </Button>
            )}
          </EmptyState>
        ) : null}

        {!loading && !errorMessage && entries.length > 0 ? (
          <div className="mt-4">
            <div className="mb-2 flex flex-wrap items-baseline justify-between gap-2">
              <p className="min-w-0 text-[11px] text-muted" aria-live="polite">
                {t('audit.summary.loaded', {
                  count: formatNumber(entries.length),
                  filters: describeFilters(applied),
                  limit: formatNumber(applied.limit),
                })}
              </p>
              <span className="text-[11px] text-subtle">
                {nextCursor ? t('audit.summary.more') : t('audit.summary.end')}
              </span>
            </div>

            <AuditList entries={entries} />

            {moreErrorMessage ? (
              <Alert status="danger" className="mt-3">
                <Alert.Indicator>
                  <AlertIcon aria-hidden="true" className="size-4" />
                </Alert.Indicator>
                <Alert.Content>
                  <Alert.Description>{moreErrorMessage}</Alert.Description>
                </Alert.Content>
              </Alert>
            ) : null}

            <div className="mt-3 flex justify-center">
              {nextCursor ? (
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-9 px-4"
                  isPending={loadingMore}
                  isDisabled={refreshing}
                  onPress={() => void loadMore()}
                >
                  {loadingMore ? null : t('audit.summary.loadMore')}
                </Button>
              ) : (
                <p className="py-2 text-[11px] text-subtle">{t('audit.summary.loadedAll')}</p>
              )}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  )
}
