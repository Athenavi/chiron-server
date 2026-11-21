'use client'

/**
 * 用量明细：按模型 / 按成员 / 按工具三个标签页。
 *
 * - 标签用 HeroUI Tabs（**不渲染 `Tabs.Indicator`**，该子组件已知会崩）；
 * - 每个标签都是「桌面语义表格 + 移动端卡片」双布局；
 * - 单个标签没有数据时给一块轻量空态，不影响其它标签；
 * - 表头 / caption / aria 与卡片字段名全部走 i18n，数字按 locale 用 `Intl`。
 */

import { useState } from 'react'
import { Tabs } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import type { UsageModelRow, UsageReport, UsageToolRow, UsageUserRow } from '@/lib/types'
import { formatCost, formatCount, formatDuration } from './usage-format'

const TABS = [
  { id: 'model', labelKey: 'usage.breakdown.byModel' },
  { id: 'user', labelKey: 'usage.breakdown.byUser' },
  { id: 'tool', labelKey: 'usage.breakdown.byTool' },
] as const

type TabKey = (typeof TABS)[number]['id']

/** 表头样式（与成员列表保持一致）。 */
const HEAD_CELL = 'px-3 py-2.5 font-medium'
const NUM_HEAD_CELL = 'px-3 py-2.5 text-right font-medium'
const NUM_CELL = 'px-3 py-3 text-right text-[13px] text-foreground tabular'
const NAME_CELL = 'px-4 py-3'

function PanelEmpty({ text }: { text: string }) {
  return (
    <p className="rounded-xl border border-dashed border-border bg-surface px-4 py-10 text-center text-xs text-muted">
      {text}
    </p>
  )
}

function MobileStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="text-[11px] text-muted">{label}</dt>
      <dd className="text-[12px] font-medium text-foreground tabular">{value}</dd>
    </div>
  )
}

/** 输入 / 输出明细副行（表格与卡片共用）。 */
function TokenHint({ input, output }: { input: string; output: string }) {
  const { t } = useI18n()
  return (
    <span className="mt-0.5 block text-[11px] font-normal text-subtle tabular">
      {t('usage.overview.input', { n: input })} · {t('usage.overview.output', { n: output })}
    </span>
  )
}

function ModelTable({ rows }: { rows: UsageModelRow[] }) {
  const { t, locale } = useI18n()
  if (rows.length === 0) return <PanelEmpty text={t('usage.breakdown.modelEmpty')} />

  return (
    <>
      <div className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
        <table className="w-full border-collapse text-left text-sm" aria-label={t('usage.breakdown.modelLabel')}>
          <caption className="sr-only">{t('usage.breakdown.modelCaption')}</caption>
          <thead>
            <tr className="border-b border-border bg-surface-secondary/60 text-[11px] tracking-wide text-subtle">
              <th scope="col" className={HEAD_CELL}>
                {t('usage.breakdown.colModel')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colRuns')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colSessions')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colTokens')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colCost')}
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {rows.map((row) => (
              <tr key={row.model} className="transition-colors hover:bg-surface-hover/60">
                <td className={NAME_CELL}>
                  <span className="block max-w-[24ch] truncate font-mono text-[12px] text-foreground" title={row.model}>
                    {row.model}
                  </span>
                </td>
                <td className={NUM_CELL}>{formatCount(row.runs, locale)}</td>
                <td className={NUM_CELL}>{formatCount(row.sessions, locale)}</td>
                <td className={NUM_CELL}>
                  {formatCount(row.input_tokens + row.output_tokens, locale)}
                  <TokenHint
                    input={formatCount(row.input_tokens, locale)}
                    output={formatCount(row.output_tokens, locale)}
                  />
                </td>
                <td className={NUM_CELL}>{formatCost(row.cost, locale)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <ul className="flex flex-col gap-2 md:hidden" aria-label={t('usage.breakdown.modelLabel')}>
        {rows.map((row) => (
          <li key={row.model} className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
            <p className="truncate font-mono text-[12px] text-foreground" title={row.model}>
              {row.model}
            </p>
            <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1">
              <MobileStat label={t('usage.breakdown.colRuns')} value={formatCount(row.runs, locale)} />
              <MobileStat
                label={t('usage.breakdown.colSessions')}
                value={formatCount(row.sessions, locale)}
              />
              <MobileStat
                label={t('usage.breakdown.colTokens')}
                value={formatCount(row.input_tokens + row.output_tokens, locale)}
              />
              <MobileStat label={t('usage.breakdown.colCost')} value={formatCost(row.cost, locale)} />
            </dl>
            <p className="mt-1.5 text-[11px] text-subtle tabular">
              {t('usage.overview.input', { n: formatCount(row.input_tokens, locale) })} ·{' '}
              {t('usage.overview.output', { n: formatCount(row.output_tokens, locale) })}
            </p>
          </li>
        ))}
      </ul>
    </>
  )
}

function userDisplayName(row: UsageUserRow): string {
  return row.nickname?.trim() || row.email?.trim() || row.user_id
}

function userDetail(row: UsageUserRow): string {
  if (row.nickname?.trim() && row.email?.trim()) return row.email
  return row.user_id
}

function UserTable({ rows }: { rows: UsageUserRow[] }) {
  const { t, locale } = useI18n()
  if (rows.length === 0) return <PanelEmpty text={t('usage.breakdown.userEmpty')} />

  return (
    <>
      <div className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
        <table className="w-full border-collapse text-left text-sm" aria-label={t('usage.breakdown.userLabel')}>
          <caption className="sr-only">{t('usage.breakdown.userCaption')}</caption>
          <thead>
            <tr className="border-b border-border bg-surface-secondary/60 text-[11px] tracking-wide text-subtle">
              <th scope="col" className={HEAD_CELL}>
                {t('usage.breakdown.colUser')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colRuns')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colSessions')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colTokens')}
              </th>
              <th scope="col" className={NUM_HEAD_CELL}>
                {t('usage.breakdown.colCost')}
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {rows.map((row) => (
              <tr key={row.user_id} className="transition-colors hover:bg-surface-hover/60">
                <td className={NAME_CELL}>
                  <span className="block max-w-[22ch] truncate text-[13px] font-medium text-foreground">
                    {userDisplayName(row)}
                  </span>
                  <p className="mt-0.5 max-w-[26ch] truncate text-[11px] text-muted">
                    {userDetail(row)}
                  </p>
                </td>
                <td className={NUM_CELL}>{formatCount(row.runs, locale)}</td>
                <td className={NUM_CELL}>{formatCount(row.sessions, locale)}</td>
                <td className={NUM_CELL}>
                  {formatCount(row.input_tokens + row.output_tokens, locale)}
                  <TokenHint
                    input={formatCount(row.input_tokens, locale)}
                    output={formatCount(row.output_tokens, locale)}
                  />
                </td>
                <td className={NUM_CELL}>{formatCost(row.cost, locale)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <ul className="flex flex-col gap-2 md:hidden" aria-label={t('usage.breakdown.userLabel')}>
        {rows.map((row) => (
          <li key={row.user_id} className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
            <p className="truncate text-[13px] font-medium text-foreground">{userDisplayName(row)}</p>
            <p className="truncate text-[11px] text-muted">{userDetail(row)}</p>
            <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1">
              <MobileStat label={t('usage.breakdown.colRuns')} value={formatCount(row.runs, locale)} />
              <MobileStat
                label={t('usage.breakdown.colSessions')}
                value={formatCount(row.sessions, locale)}
              />
              <MobileStat
                label={t('usage.breakdown.colTokens')}
                value={formatCount(row.input_tokens + row.output_tokens, locale)}
              />
              <MobileStat label={t('usage.breakdown.colCost')} value={formatCost(row.cost, locale)} />
            </dl>
          </li>
        ))}
      </ul>
    </>
  )
}

function ToolTable({
  rows,
  totals,
}: {
  rows: UsageToolRow[]
  totals: UsageReport['tools']['totals']
}) {
  const { t, locale } = useI18n()

  return (
    <>
      <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <div className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
          <dt className="text-[11px] text-muted">{t('usage.breakdown.callsTotal')}</dt>
          <dd className="mt-1 truncate text-lg font-semibold text-foreground tabular">
            {formatCount(totals.calls, locale)}
          </dd>
        </div>
        <div className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
          <dt className="text-[11px] text-muted">{t('usage.breakdown.failures')}</dt>
          <dd className="mt-1 truncate text-lg font-semibold text-foreground tabular">
            {formatCount(totals.failures, locale)}
          </dd>
        </div>
        <div className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
          <dt className="text-[11px] text-muted">{t('usage.breakdown.avg')}</dt>
          <dd className="mt-1 truncate text-lg font-semibold text-foreground tabular">
            {formatDuration(totals.avg_ms, locale)}
          </dd>
        </div>
        <div className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
          <dt className="text-[11px] text-muted">{t('usage.breakdown.max')}</dt>
          <dd className="mt-1 truncate text-lg font-semibold text-foreground tabular">
            {formatDuration(totals.max_ms, locale)}
          </dd>
        </div>
      </dl>

      {rows.length === 0 ? (
        <PanelEmpty text={t('usage.breakdown.toolEmpty')} />
      ) : (
        <>
          <div className="mt-3 hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
            <table className="w-full border-collapse text-left text-sm" aria-label={t('usage.breakdown.toolLabel')}>
              <caption className="sr-only">{t('usage.breakdown.toolCaption')}</caption>
              <thead>
                <tr className="border-b border-border bg-surface-secondary/60 text-[11px] tracking-wide text-subtle">
                  <th scope="col" className={HEAD_CELL}>
                    {t('usage.breakdown.colTool')}
                  </th>
                  <th scope="col" className={NUM_HEAD_CELL}>
                    {t('usage.breakdown.colCalls')}
                  </th>
                  <th scope="col" className={NUM_HEAD_CELL}>
                    {t('usage.breakdown.colFailures')}
                  </th>
                  <th scope="col" className={NUM_HEAD_CELL}>
                    {t('usage.breakdown.colAvg')}
                  </th>
                  <th scope="col" className={NUM_HEAD_CELL}>
                    {t('usage.breakdown.colMax')}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {rows.map((row) => (
                  <tr key={row.name} className="transition-colors hover:bg-surface-hover/60">
                    <td className={NAME_CELL}>
                      <span
                        className="block max-w-[24ch] truncate font-mono text-[12px] text-foreground"
                        title={row.name}
                      >
                        {row.name}
                      </span>
                    </td>
                    <td className={NUM_CELL}>{formatCount(row.calls, locale)}</td>
                    <td className={NUM_CELL}>{formatCount(row.failures, locale)}</td>
                    <td className={NUM_CELL}>{formatDuration(row.avg_ms, locale)}</td>
                    <td className={NUM_CELL}>{formatDuration(row.max_ms, locale)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <ul className="mt-3 flex flex-col gap-2 md:hidden" aria-label={t('usage.breakdown.toolLabel')}>
            {rows.map((row) => (
              <li key={row.name} className="min-w-0 rounded-xl border border-border bg-surface p-3 shadow-xs">
                <p className="truncate font-mono text-[12px] text-foreground" title={row.name}>
                  {row.name}
                </p>
                <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1">
                  <MobileStat label={t('usage.breakdown.colCalls')} value={formatCount(row.calls, locale)} />
                  <MobileStat
                    label={t('usage.breakdown.colFailures')}
                    value={formatCount(row.failures, locale)}
                  />
                  <MobileStat
                    label={t('usage.breakdown.colAvg')}
                    value={formatDuration(row.avg_ms, locale)}
                  />
                  <MobileStat
                    label={t('usage.breakdown.colMax')}
                    value={formatDuration(row.max_ms, locale)}
                  />
                </dl>
              </li>
            ))}
          </ul>
        </>
      )}
    </>
  )
}

export function UsageBreakdown({ report }: { report: UsageReport }) {
  const { t } = useI18n()
  const [tab, setTab] = useState<TabKey>('model')

  return (
    <Tabs
      selectedKey={tab}
      onSelectionChange={(key) => {
        if (key !== null) setTab(String(key) as TabKey)
      }}
      variant="secondary"
      className="mt-4"
    >
      <Tabs.ListContainer className="w-full min-w-0">
        <Tabs.List aria-label={t('usage.breakdown.tabsLabel')}>
          {TABS.map((item) => (
            <Tabs.Tab key={item.id} id={item.id}>
              {t(item.labelKey)}
            </Tabs.Tab>
          ))}
        </Tabs.List>
      </Tabs.ListContainer>

      <Tabs.Panel id="model" className="pt-4">
        <ModelTable rows={report.by_model} />
      </Tabs.Panel>
      <Tabs.Panel id="user" className="pt-4">
        <UserTable rows={report.by_user} />
      </Tabs.Panel>
      <Tabs.Panel id="tool" className="pt-4">
        <ToolTable rows={report.tools.by_tool} totals={report.tools.totals} />
      </Tabs.Panel>
    </Tabs>
  )
}
