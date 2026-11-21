'use client'

import type { Key } from 'react'
import { Dropdown, IconChevronDown } from '@heroui/react'
import { cn } from '@/lib/utils'
import { useTheme } from './ThemeProvider'
import { THEME_OPTIONS, isThemePreference } from './theme-config'
import { useI18n } from '@/lib/i18n'

/**
 * 主题切换器：HeroUI Dropdown（单选菜单）。
 *
 * 每一项带色块 + 名称，选中项显示勾选标记；触发器只显示当前主题色块与工具提示文案，
 * 因此 SSR 与首帧客户端渲染完全一致，不会触发 hydration 不匹配。
 */
export function ThemeSwitcher({ className }: { className?: string }) {
  const { theme, resolved, setTheme } = useTheme()
  const { t } = useI18n()
  const current = THEME_OPTIONS.find((option) => option.id === theme) ?? THEME_OPTIONS[0]
  const label = t('theme.label', {
    name: t(`theme.${theme === 'system' ? 'system' : resolved}`),
  })

  function handleSelectionChange(keys: 'all' | Set<Key>) {
    if (keys === 'all') return
    const [first] = Array.from(keys)
    if (typeof first === 'string' && isThemePreference(first)) setTheme(first)
  }

  return (
    <Dropdown>
      <Dropdown.Trigger
        aria-label={label}
        className={cn(
          'inline-flex h-10 items-center justify-center gap-1.5 rounded-full border border-border sm:h-9',
          'bg-surface px-2.5 text-foreground shadow-field transition-colors hover:bg-surface-hover',
          className,
        )}
      >
        <span
          aria-hidden="true"
          className="size-3.5 shrink-0 rounded-full ring-1 ring-border"
          style={{ background: current.swatch }}
        />
        <IconChevronDown aria-hidden="true" className="size-3.5 opacity-60" />
      </Dropdown.Trigger>

      <Dropdown.Popover placement="bottom end">
        <Dropdown.Menu
          aria-label={t('theme.title')}
          selectionMode="single"
          selectedKeys={[theme]}
          onSelectionChange={handleSelectionChange}
        >
          {THEME_OPTIONS.map((option) => (
            <Dropdown.Item
              key={option.id}
              id={option.id}
              textValue={t(`theme.${option.id}`)}
            >
              <span
                aria-hidden="true"
                className="size-3 shrink-0 rounded-full ring-1 ring-border"
                style={{ background: option.swatch }}
              />
              <span className="flex min-w-0 flex-1 flex-col">
                <span className="truncate">{t(`theme.${option.id}`)}</span>
                <span className="truncate text-xs text-muted">
                  {t(`theme.${option.id}Desc`)}
                </span>
              </span>
              <Dropdown.ItemIndicator />
            </Dropdown.Item>
          ))}
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
  )
}
