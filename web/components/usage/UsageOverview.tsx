'use client'

/**
 * 用量概览：四张统计卡片（运行 / 会话 / token / 成本）+ 套餐上限对照。
 *
 * - 卡片在移动端 2×2、桌面端一行四列，数字统一等宽（`.tabular`）避免跳动；
 * - token 卡带输入 / 输出明细，成本卡带缓存命中明细；
 * - 套餐存在时展示进度条；上限为 0（或后端没下发成本上限）显示「不限」。
 * - 文案全部走 i18n，数字 / 货币 / 百分比按 locale 用 `Intl` 格式化。
 */

import { useI18n } from '@/lib/i18n'
import type { Plan, UsageReport } from '@/lib/types'
import { cn } from '@/lib/utils'
import {
  costQuota,
  formatCost,
  formatCount,
  formatPercent,
  quotaPercent,
  tokenQuota,
  type QuotaLine,
} from './usage-format'

interface StatCardProps {
  label: string
  value: string
  hint?: string
}

function StatCard({ label, value, hint }: StatCardProps) {
  return (
    <div className="min-w-0 rounded-xl border border-border bg-surface p-3.5 shadow-xs md:p-4">
      <p className="truncate text-[11px] font-medium uppercase tracking-[0.08em] text-subtle">
        {label}
      </p>
      <p className="mt-1.5 truncate text-lg font-semibold tracking-tight text-foreground tabular md:text-2xl">
        {value}
      </p>
      {hint ? <p className="mt-1 truncate text-[11px] leading-4 text-muted tabular">{hint}</p> : null}
    </div>
  )
}

function QuotaBar({ quota }: { quota: QuotaLine }) {
  const { t, locale } = useI18n()
  const percent = quotaPercent(quota)
  const percentText = formatPercent(percent, locale)
  const label =
    quota.id === 'token' ? t('usage.overview.tokenQuota') : t('usage.overview.costQuota')

  return (
    <div className="min-w-0">
      <div className="flex items-baseline justify-between gap-2">
        <span className="min-w-0 truncate text-[12px] text-muted">{label}</span>
        <span className="shrink-0 text-[12px] font-medium text-foreground tabular">
          {quota.format(quota.used)}
          {quota.bounded
            ? ` / ${quota.format(quota.limit)}`
            : ` · ${t('usage.overview.unlimited')}`}
        </span>
      </div>

      {quota.bounded ? (
        <>
          <div
            className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-surface-secondary"
            role="progressbar"
            aria-label={label}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(percent)}
            aria-valuetext={t('usage.overview.used', { percent: percentText })}
          >
            <div
              className={cn(
                'h-full rounded-full transition-[width] duration-300',
                percent >= 90 ? 'bg-danger' : percent >= 70 ? 'bg-warning' : 'bg-accent',
              )}
              style={{ width: `${percent}%` }}
            />
          </div>
          <p className="mt-1 text-[11px] text-subtle tabular">
            {t('usage.overview.used', { percent: percentText })}
            {percent >= 90 ? ` · ${t('usage.overview.nearLimit')}` : ''}
          </p>
        </>
      ) : (
        <p className="mt-2 text-[11px] text-subtle">{t('usage.overview.unbounded')}</p>
      )}
    </div>
  )
}

export function UsageOverview({ report, plan }: { report: UsageReport; plan: Plan | null }) {
  const { t, locale } = useI18n()
  const { totals, tools } = report
  const quotas = [tokenQuota(report, plan, locale), costQuota(report, plan, locale)].filter(
    (quota): quota is QuotaLine => quota !== null,
  )

  const avgTokens = totals.runs > 0 ? totals.total_tokens / totals.runs : 0

  return (
    <section aria-label={t('usage.overview.label')} className="flex flex-col gap-3">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatCard
          label={t('usage.overview.runs')}
          value={formatCount(totals.runs, locale)}
          hint={t('usage.overview.runsHint', { n: formatCount(tools.totals.calls, locale) })}
        />
        <StatCard
          label={t('usage.overview.sessions')}
          value={formatCount(totals.sessions, locale)}
          hint={t('usage.overview.sessionsHint', { n: formatCount(avgTokens, locale) })}
        />
        <StatCard
          label={t('usage.overview.tokens')}
          value={formatCount(totals.total_tokens, locale)}
          hint={t('usage.overview.tokensHint', {
            input: formatCount(totals.input_tokens, locale),
            output: formatCount(totals.output_tokens, locale),
          })}
        />
        <StatCard
          label={t('usage.overview.cost')}
          value={formatCost(totals.cost, locale)}
          hint={t('usage.overview.costHint', {
            n: formatCount(totals.cache_read_input_tokens, locale),
          })}
        />
      </div>

      {plan && quotas.length > 0 ? (
        <div className="rounded-xl border border-border bg-surface p-3.5 shadow-xs md:p-4">
          <div className="flex items-baseline justify-between gap-3">
            <h2 className="shrink-0 text-[13px] font-semibold text-foreground">
              {t('usage.overview.quotaTitle')}
            </h2>
            <span className="min-w-0 truncate text-[11px] text-muted">
              {t('usage.overview.quotaPlan', { id: plan.plan_id })}
            </span>
          </div>
          <div className="mt-3 grid gap-4 sm:grid-cols-2">
            {quotas.map((quota) => (
              <QuotaBar key={quota.id} quota={quota} />
            ))}
          </div>
          <p className="mt-3 text-[11px] leading-4 text-subtle">{t('usage.overview.note')}</p>
        </div>
      ) : null}
    </section>
  )
}
