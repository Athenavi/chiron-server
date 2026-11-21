'use client'

/**
 * 用量页（仅 owner / admin）。
 *
 * - 非 owner/admin 直接展示无权限空态，不发任何请求；
 * - 页头提供窗口 Select（近 1 / 7 / 30 / 90 天）与刷新；切换窗口即重拉，
 *   请求序号保证慢响应不会覆盖新窗口的数据；
 * - 概览卡片（运行 / 会话 / token / 成本）在套餐存在时附带上限进度条；
 * - 明细用三个标签页（按模型 / 按成员 / 按工具），桌面表格 + 移动端卡片；
 * - 加载骨架、空态、错误 Alert + 重试齐备；
 * - 文案全部走 `usage.*` i18n 命名空间，时间 / 数字按 locale 用 `Intl`。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import Link from 'next/link'
import { Alert, Button, EmptyState, Label, ListBox, Select } from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { PageHeader } from '@/components/shell/PageHeader'
import { UsageBreakdown } from '@/components/usage/UsageBreakdown'
import { UsageOverview } from '@/components/usage/UsageOverview'
import { UsageSkeleton } from '@/components/usage/UsageSkeleton'
import {
  DEFAULT_WINDOW_DAYS,
  WINDOW_OPTIONS,
  formatWindowRange,
  usageIsEmpty,
} from '@/components/usage/usage-format'
import {
  AlertIcon,
  LockIcon,
  MessageIcon,
  RefreshIcon,
  SparkIcon,
} from '@/components/ui-bridge/icons'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type UsageReport } from '@/lib/types'

/** owner / admin 才有权查看租户用量。 */
function isAdminRole(role: string | null | undefined): boolean {
  const key = (role ?? '').trim().toLowerCase()
  return key === 'owner' || key === 'admin'
}

export default function UsagePage() {
  const { t, locale } = useI18n()
  const { user, plan } = useAuth()
  const isAdmin = isAdminRole(user?.role)

  const [days, setDays] = useState(DEFAULT_WINDOW_DAYS)
  const [report, setReport] = useState<UsageReport | null>(null)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  /** 保留原始错误对象：错误文案在渲染时按当前 locale 生成，切换语言不会重新拉取。 */
  const [error, setError] = useState<unknown>(null)

  /** 请求序号：窗口切换很快时只接受最后一次响应。 */
  const requestSeq = useRef(0)

  const load = useCallback(
    async (windowDays: number, options: { silent?: boolean } = {}) => {
      const seq = (requestSeq.current += 1)
      if (options.silent) setRefreshing(true)
      else setLoading(true)
      setError(null)
      try {
        const result = await api.getUsage({ days: windowDays })
        if (seq !== requestSeq.current) return
        setReport(result)
      } catch (cause) {
        if (seq !== requestSeq.current) return
        setError(cause)
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
    void load(days)
  }, [isAdmin, days, load])

  // ── 无权限 ────────────────────────────────────────────────────────────

  if (!isAdmin) {
    return (
      <div>
        <PageHeader title={t('usage.title')} description={t('usage.restrictedDescription')} />
        <div className="content-shell max-w-5xl py-6 md:py-8">
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <LockIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('usage.noAccess.title')}</h2>
            <p className="max-w-md text-xs leading-5 text-muted">{t('usage.noAccess.body')}</p>
            <Link
              href="/sessions"
              className="mt-1 inline-flex h-9 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary-hover"
            >
              <MessageIcon size={15} />
              {t('usage.noAccess.back')}
            </Link>
          </EmptyState>
        </div>
      </div>
    )
  }

  // ── 有权限 ────────────────────────────────────────────────────────────

  const windowLabel = (value: number) =>
    value === 1 ? t('usage.window.lastDay') : t('usage.window.lastDays', { n: value })
  const windowText = report
    ? formatWindowRange(report.window.from, report.window.to, locale)
    : windowLabel(days)
  const planText = plan ? t('usage.plan.bound', { id: plan.plan_id }) : t('usage.plan.none')
  const description = t('usage.headerSummary', { window: windowText, plan: planText })
  const errorMessage = error ? ApiError.from(error, t('usage.error.fallback')).message : null
  const empty = !loading && !errorMessage && usageIsEmpty(report)

  return (
    <div>
      <PageHeader
        title={t('usage.title')}
        description={description}
        actions={
          <>
            <Select
              aria-label={t('usage.window.label')}
              selectedKey={String(days)}
              isDisabled={loading}
              onSelectionChange={(key) => {
                if (key === null) return
                const next = Number(key)
                if (Number.isFinite(next) && next > 0) setDays(next)
              }}
              className="w-28 sm:w-32"
            >
              <Label className="sr-only">{t('usage.window.label')}</Label>
              <Select.Trigger className="h-9">
                <Select.Value />
                <Select.Indicator />
              </Select.Trigger>
              <Select.Popover>
                <ListBox aria-label={t('usage.window.label')}>
                  {WINDOW_OPTIONS.map((value) => (
                    <ListBox.Item
                      key={value}
                      id={String(value)}
                      textValue={windowLabel(value)}
                    >
                      {windowLabel(value)}
                      <ListBox.Item.Indicator />
                    </ListBox.Item>
                  ))}
                </ListBox>
              </Select.Popover>
            </Select>

            <Button
              variant="secondary"
              size="sm"
              className="h-9 gap-1.5"
              isPending={refreshing}
              isDisabled={loading}
              onPress={() => void load(days, { silent: true })}
            >
              {refreshing ? null : <RefreshIcon size={14} />}
              {t('common.refresh')}
            </Button>
          </>
        }
      />

      <div className="content-shell max-w-5xl py-6 md:py-8">
        {errorMessage ? (
          <Alert status="danger" className="mb-4">
            <Alert.Indicator>
              <AlertIcon aria-hidden="true" className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>{t('usage.error.title')}</Alert.Title>
              <Alert.Description>{errorMessage}</Alert.Description>
            </Alert.Content>
            <Button
              variant="secondary"
              size="sm"
              className="h-8 shrink-0 gap-1.5 self-center"
              onPress={() => void load(days)}
            >
              <RefreshIcon size={14} />
              {t('common.retry')}
            </Button>
          </Alert>
        ) : null}

        {loading ? <UsageSkeleton /> : null}

        {empty ? (
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <SparkIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('usage.empty.title')}</h2>
            <p className="max-w-sm text-xs leading-5 text-muted">
              {t('usage.empty.body', { n: days })}
            </p>
            <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
              <Button
                variant="primary"
                className="h-9 gap-1.5"
                onPress={() => {
                  const next = WINDOW_OPTIONS.find((option) => option > days)
                  if (next !== undefined) setDays(next)
                  else void load(days)
                }}
              >
                {t('usage.empty.widen')}
              </Button>
              <Button variant="ghost" className="h-9 px-3" onPress={() => void load(days)}>
                {t('usage.empty.refresh')}
              </Button>
            </div>
          </EmptyState>
        ) : null}

        {!loading && !errorMessage && report && !empty ? (
          <>
            <UsageOverview report={report} plan={plan} />
            <UsageBreakdown report={report} />
          </>
        ) : null}
      </div>
    </div>
  )
}
