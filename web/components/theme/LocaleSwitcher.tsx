'use client'

import type { Key } from 'react'
import { Dropdown, IconChevronDown } from '@heroui/react'
import { useI18n, type Locale } from '@/lib/i18n'
import { cn } from '@/lib/utils'

export function LocaleSwitcher({ className }: { className?: string }) {
  const { locale, setLocale, t } = useI18n()

  function onChange(keys: 'all' | Set<Key>) {
    if (keys === 'all') return
    const [first] = Array.from(keys)
    if (first === 'zh' || first === 'en') setLocale(first as Locale)
  }

  return (
    <Dropdown>
      <Dropdown.Trigger
        aria-label={`${t('theme.locale')}: ${locale === 'en' ? 'English' : '中文'}`}
        className={cn(
          'inline-flex h-10 items-center justify-center gap-1.5 rounded-full border border-border sm:h-9',
          'bg-surface px-2.5 text-foreground shadow-field transition-colors hover:bg-surface-hover',
          className,
        )}
      >
        <span aria-hidden="true" className="text-xs font-medium">
          {locale === 'en' ? 'EN' : '中'}
        </span>
        <IconChevronDown aria-hidden="true" className="size-3.5 opacity-60" />
      </Dropdown.Trigger>
      <Dropdown.Popover placement="bottom end">
        <Dropdown.Menu
          aria-label={t('theme.locale')}
          selectionMode="single"
          selectedKeys={[locale]}
          onSelectionChange={onChange}
        >
          <Dropdown.Item id="zh" key="zh" textValue={t('theme.zh')}>
            {t('theme.zh')}
            <Dropdown.ItemIndicator />
          </Dropdown.Item>
          <Dropdown.Item id="en" key="en" textValue={t('theme.en')}>
            {t('theme.en')}
            <Dropdown.ItemIndicator />
          </Dropdown.Item>
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
  )
}
