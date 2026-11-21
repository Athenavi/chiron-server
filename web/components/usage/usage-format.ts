/**
 * 用量页的纯展示工具：数字 / 成本 / 耗时格式化，以及套餐上限换算。
 *
 * 只做格式化与派生计算，不发请求；页面、概览卡片与明细表共用同一套规则，
 * 保证同一个数字在概览、明细和页头里长得完全一样。
 *
 * 所有格式化都按当前 locale 走 `Intl`（`zh-CN` / `en-US`），文案单位（「近 N 天」
 * 「共 N 条」等）由调用方通过 i18n 拼接，这里只负责数字本身。
 */

import type { Locale } from '@/lib/i18n'
import type { Plan, UsageReport } from '@/lib/types'

/** 统计窗口（与后端 `days` 语义一致：1 / 7 / 30 / 90，缺省 7 天）。 */
export const WINDOW_OPTIONS: readonly number[] = [1, 7, 30, 90]

export const DEFAULT_WINDOW_DAYS = 7

/** 缺失值的统一占位。 */
const PLACEHOLDER = '—'

function intlTag(locale: Locale): string {
  return locale === 'en' ? 'en-US' : 'zh-CN'
}

/** 千分位整数；非法值回落 `—`。 */
export function formatCount(value: number | null | undefined, locale: Locale): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return PLACEHOLDER
  return new Intl.NumberFormat(intlTag(locale), { maximumFractionDigits: 0 }).format(
    Math.round(value),
  )
}

/** 成本：美元；不足 1 美元时用 4 位小数，避免小额显示成 `$0.00`。 */
export function formatCost(value: number | null | undefined, locale: Locale): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return PLACEHOLDER
  const abs = Math.abs(value)
  const digits = abs > 0 && abs < 1 ? 4 : 2
  return new Intl.NumberFormat(intlTag(locale), {
    style: 'currency',
    currency: 'USD',
    currencyDisplay: 'narrowSymbol',
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  }).format(value)
}

/** 耗时：毫秒 → 秒（`1.44s` / `1.44秒`），非正值回落 `—`。 */
export function formatDuration(value: number | null | undefined, locale: Locale): string {
  if (value === null || value === undefined || !Number.isFinite(value) || value <= 0) {
    return PLACEHOLDER
  }
  const seconds = value / 1000
  const fractionDigits = seconds < 10 ? 2 : 1
  const formatter = (input: number) =>
    new Intl.NumberFormat(intlTag(locale), {
      style: 'unit',
      unit: 'second',
      unitDisplay: 'narrow',
      minimumFractionDigits: fractionDigits,
      maximumFractionDigits: fractionDigits,
    }).format(input)
  if (seconds < 0.01) return `<${formatter(0.01)}`
  return formatter(seconds)
}

/** 已用百分比：保留 1 位小数，交给 i18n 模板拼 `%`。 */
export function formatPercent(value: number, locale: Locale): string {
  if (!Number.isFinite(value)) return '0'
  return new Intl.NumberFormat(intlTag(locale), {
    minimumFractionDigits: 1,
    maximumFractionDigits: 1,
    useGrouping: false,
  }).format(value)
}

/** 时间戳：本地时区的 `MM-DD HH:mm`，按 locale 取分隔符。 */
export function formatTimestamp(value: string | null | undefined, locale: Locale): string {
  if (!value) return PLACEHOLDER
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return PLACEHOLDER
  return new Intl.DateTimeFormat(intlTag(locale), {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(date)
}

/** 统计窗口区间：`开始 — 结束`。 */
export function formatWindowRange(from: string, to: string, locale: Locale): string {
  return `${formatTimestamp(from, locale)} — ${formatTimestamp(to, locale)}`
}

/**
 * 套餐里的月度成本上限（分）。
 *
 * 契约里的 `Plan` 目前没有暴露这个字段，而后端可能已经下发；这里按可选字段读取，
 * 缺失时返回 `null`（页面按「不限」处理）。
 */
export function planMonthlyCostCents(plan: Plan | null): number | null {
  if (!plan) return null
  const raw = (plan as Plan & { monthly_cost_cents?: unknown }).monthly_cost_cents
  if (typeof raw !== 'number' || !Number.isFinite(raw)) return null
  return raw
}

/** 一条配额：`limit <= 0`（或未配置）代表不限。 */
export interface QuotaLine {
  /** 配额种类：文案由界面按 i18n 键映射。 */
  id: 'token' | 'cost'
  used: number
  limit: number
  bounded: boolean
  format: (value: number) => string
}

/** token 配额（上限取套餐的 `monthly_tokens`，0 表示不限）。 */
export function tokenQuota(
  report: UsageReport | null,
  plan: Plan | null,
  locale: Locale,
): QuotaLine | null {
  if (!plan) return null
  const limit = Number.isFinite(plan.monthly_tokens) ? plan.monthly_tokens : 0
  return {
    id: 'token',
    used: report?.totals.total_tokens ?? 0,
    limit,
    bounded: limit > 0,
    format: (value) => formatCount(value, locale),
  }
}

/** 成本配额（上限取 `monthly_cost_cents / 100`，0 或缺失表示不限）。 */
export function costQuota(
  report: UsageReport | null,
  plan: Plan | null,
  locale: Locale,
): QuotaLine | null {
  if (!plan) return null
  const cents = planMonthlyCostCents(plan)
  const limit = cents === null ? 0 : cents / 100
  return {
    id: 'cost',
    used: report?.totals.cost ?? 0,
    limit,
    bounded: limit > 0,
    format: (value) => formatCost(value, locale),
  }
}

/** 已用百分比（0..100，向上限截断）；无限额时返回 0。 */
export function quotaPercent(quota: QuotaLine): number {
  if (!quota.bounded || quota.limit <= 0) return 0
  const ratio = (quota.used / quota.limit) * 100
  if (!Number.isFinite(ratio) || ratio <= 0) return 0
  return Math.min(100, ratio)
}

/** 窗口内是否完全没有用量（三个维度都为空且没有运行）。 */
export function usageIsEmpty(report: UsageReport | null): boolean {
  if (!report) return true
  return (
    report.totals.runs === 0 &&
    report.by_model.length === 0 &&
    report.by_user.length === 0 &&
    report.tools.by_tool.length === 0
  )
}
