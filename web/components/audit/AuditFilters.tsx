'use client'

/**
 * 审计过滤区：动作关键字、起止日期、每页条数。
 *
 * - 关键字用 TextField + Input（回车或「应用」按钮触发）；
 * - 日期用原生 `<input type="date">`（不额外引入日期库），按本地时区换算成
 *   RFC3339 后交给接口；起止倒置时给出提示并禁用「应用」；
 * - 「重置」把草稿恢复成默认值（动作清空、日期清空、每页 20）；
 * - 全部文案（含 aria-label）走 `audit.filters.*` i18n。
 */

import { Button, Input, Label, ListBox, Select, TextField } from '@heroui/react'
import { useI18n } from '@/lib/i18n'

export interface AuditFilterDraft {
  /** 动作关键字（模糊匹配）。 */
  action: string
  /** `yyyy-mm-dd`，空串表示不限。 */
  from: string
  /** `yyyy-mm-dd`，空串表示不限。 */
  to: string
  /** 每页条数。 */
  limit: number
}

export const AUDIT_DEFAULT_LIMIT = 20

export const EMPTY_AUDIT_FILTER: AuditFilterDraft = {
  action: '',
  from: '',
  to: '',
  limit: AUDIT_DEFAULT_LIMIT,
}

export const AUDIT_LIMIT_OPTIONS = [20, 50, 100]

/** 原生 date input 在三种主题下都沿用当前 `color-scheme`，只需统一高度与边框。 */
const DATE_INPUT_CLASS =
  'h-9 max-w-full rounded-md border border-border bg-surface px-2.5 text-[13px] text-foreground ' +
  'outline-none transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring'

export interface AuditFiltersProps {
  draft: AuditFilterDraft
  onChange: (next: AuditFilterDraft) => void
  onApply: () => void
  onReset: () => void
  /** 列表正在加载（应用 / 刷新 / 加载更多）：过滤控件暂时不可编辑。 */
  isBusy?: boolean
}

export function AuditFilters({
  draft,
  onChange,
  onApply,
  onReset,
  isBusy = false,
}: AuditFiltersProps) {
  const { t } = useI18n()
  const rangeInvalid = Boolean(draft.from && draft.to && draft.from > draft.to)

  return (
    <form
      aria-label={t('audit.filters.label')}
      className="rounded-xl border border-border bg-surface p-3 shadow-xs md:p-4"
      onSubmit={(event) => {
        event.preventDefault()
        if (rangeInvalid || isBusy) return
        onApply()
      }}
    >
      <div className="flex flex-wrap items-end gap-2.5">
        <TextField
          aria-label={t('audit.filters.action')}
          value={draft.action}
          isDisabled={isBusy}
          onChange={(value) => onChange({ ...draft, action: value })}
          className="w-full sm:w-52"
        >
          <Label className="mb-1 block text-[11px] text-muted">{t('audit.filters.action')}</Label>
          <Input
            autoComplete="off"
            spellCheck={false}
            placeholder={t('audit.filters.actionPlaceholder')}
            className="h-9 text-[13px]"
          />
        </TextField>

        <div className="flex min-w-0 flex-wrap items-end gap-2">
          <label className="flex min-w-0 flex-col gap-1">
            <span className="text-[11px] text-muted">{t('audit.filters.from')}</span>
            <input
              type="date"
              aria-label={t('audit.filters.from')}
              value={draft.from}
              disabled={isBusy}
              max={draft.to || undefined}
              onChange={(event) => onChange({ ...draft, from: event.target.value })}
              className={DATE_INPUT_CLASS}
            />
          </label>
          <span aria-hidden="true" className="pb-2 text-xs text-subtle">
            {t('audit.filters.range')}
          </span>
          <label className="flex min-w-0 flex-col gap-1">
            <span className="text-[11px] text-muted">{t('audit.filters.to')}</span>
            <input
              type="date"
              aria-label={t('audit.filters.to')}
              value={draft.to}
              disabled={isBusy}
              min={draft.from || undefined}
              onChange={(event) => onChange({ ...draft, to: event.target.value })}
              className={DATE_INPUT_CLASS}
            />
          </label>
        </div>

        <Select
          aria-label={t('audit.filters.limit')}
          selectedKey={String(draft.limit)}
          isDisabled={isBusy}
          onSelectionChange={(key) => {
            if (key === null) return
            const next = Number(key)
            if (Number.isFinite(next) && next > 0) onChange({ ...draft, limit: next })
          }}
          className="w-28"
        >
          <Label className="mb-1 block text-[11px] text-muted">{t('audit.filters.limit')}</Label>
          <Select.Trigger className="h-9">
            <Select.Value />
            <Select.Indicator />
          </Select.Trigger>
          <Select.Popover>
            <ListBox aria-label={t('audit.filters.limit')}>
              {AUDIT_LIMIT_OPTIONS.map((value) => (
                <ListBox.Item
                  key={value}
                  id={String(value)}
                  textValue={t('audit.filters.limitOption', { n: value })}
                >
                  {t('audit.filters.limitOption', { n: value })}
                  <ListBox.Item.Indicator />
                </ListBox.Item>
              ))}
            </ListBox>
          </Select.Popover>
        </Select>

        <div className="flex items-center gap-2">
          <Button
            type="submit"
            variant="primary"
            size="sm"
            className="h-9 px-4"
            isDisabled={rangeInvalid || isBusy}
          >
            {t('audit.filters.apply')}
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-9 px-3"
            isDisabled={isBusy}
            onPress={onReset}
          >
            {t('audit.filters.reset')}
          </Button>
        </div>
      </div>

      {rangeInvalid ? (
        <p role="alert" className="mt-2 text-[11px] text-danger">
          {t('audit.filters.rangeInvalid')}
        </p>
      ) : null}
    </form>
  )
}
