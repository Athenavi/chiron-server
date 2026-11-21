'use client'

/**
 * 侧栏底部用户区：头像 + 昵称/邮箱 + 操作菜单（设置 / 切换主题 / 退出登录）。
 *
 * 主题入口做成「循环切换」菜单项：侧栏已经很窄，再塞一个下拉会很挤；
 * 完整主题选择在设置页（`ThemeSwitcher`）。
 */

import { useState } from 'react'
import { useRouter } from 'next/navigation'
import type { Key } from 'react'
import { Avatar, Dropdown, Spinner } from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { useTheme } from '@/components/theme/ThemeProvider'
import { THEME_OPTIONS } from '@/components/theme/theme-config'
import { useI18n } from '@/lib/i18n'
import {
  ArchiveIcon,
  ChevronDownIcon,
  LogOutIcon,
  SettingsIcon,
  SparkIcon,
} from '@/components/ui-bridge/icons'

export function SidebarUserMenu() {
  const router = useRouter()
  const { user, logout } = useAuth()
  const { theme, setTheme } = useTheme()
  const { locale, setLocale, t } = useI18n()
  const [loggingOut, setLoggingOut] = useState(false)

  const displayName = user?.nickname?.trim() || user?.name || user?.email || t('auth.member')
  const avatarLabel = displayName.trim().slice(0, 1) || '?'

  function cycleTheme() {
    const index = THEME_OPTIONS.findIndex((option) => option.id === theme)
    const next = THEME_OPTIONS[(index + 1) % THEME_OPTIONS.length]
    setTheme(next.id)
  }

  async function handleLogout() {
    if (loggingOut) return
    setLoggingOut(true)
    try {
      await logout()
    } catch {
      // 网络失败也要离开页面：本地状态已清空
    } finally {
      setLoggingOut(false)
      router.replace('/login')
    }
  }

  function handleAction(key: Key) {
    if (key === 'settings') {
      router.push('/settings')
      return
    }
    if (key === 'archived') {
      router.push('/archived')
      return
    }
    if (key === 'theme') {
      cycleTheme()
      return
    }
    if (key === 'locale') {
      setLocale(locale === 'zh' ? 'en' : 'zh')
      return
    }
    if (key === 'logout') void handleLogout()
  }

  return (
    // mt-auto：桌面折叠时会话列表被隐藏，用户区仍然贴着底部
    <div className="mt-auto shrink-0 border-t border-sidebar-border px-2 py-2">
      <Dropdown>
        <Dropdown.Trigger
          aria-label={`${t('nav.settings')}: ${displayName}`}
          className="sidebar-icon-trigger flex w-full items-center gap-2.5 rounded-md px-1.5 py-1.5 text-left transition-colors hover:bg-sidebar-hover data-[focus-visible]:outline-2 data-[focus-visible]:outline-offset-1 data-[focus-visible]:outline-ring"
        >
          <Avatar
            size="sm"
            color="default"
            aria-label={displayName}
            className="shrink-0 ring-1 ring-sidebar-border"
          >
            <Avatar.Fallback>{avatarLabel}</Avatar.Fallback>
          </Avatar>
          {/* 折叠后只留头像 */}
          <span className="sidebar-expanded-only min-w-0 flex-1">
            <span className="block truncate text-[13px] font-medium leading-5 text-sidebar-foreground">
              {displayName}
            </span>
            <span className="block truncate text-[11px] leading-4 text-sidebar-muted">
              {user?.email ?? '—'}
            </span>
          </span>
          {loggingOut ? (
            <Spinner size="sm" color="current" aria-label={t('auth.logout')} />
          ) : (
            <ChevronDownIcon
              size={14}
              className="sidebar-expanded-only shrink-0 text-sidebar-muted"
            />
          )}
        </Dropdown.Trigger>

        <Dropdown.Popover placement="top start">
          <Dropdown.Menu aria-label={t('nav.settings')} onAction={handleAction}>
            <Dropdown.Item id="settings" textValue={t('nav.settings')}>
              <SettingsIcon size={14} />
              <span className="truncate">{t('nav.settings')}</span>
            </Dropdown.Item>
            <Dropdown.Item id="archived" textValue={t('nav.archived')}>
              <ArchiveIcon size={14} />
              <span className="truncate">{t('nav.archived')}</span>
            </Dropdown.Item>
            <Dropdown.Item
              id="theme"
              textValue={t('theme.label', { name: t(`theme.${theme}`) })}
            >
              <SparkIcon size={14} />
              <span className="truncate">
                {t('theme.label', { name: t(`theme.${theme}`) })}
              </span>
            </Dropdown.Item>
            <Dropdown.Item
              id="locale"
              textValue={t('theme.locale')}
            >
              <span aria-hidden="true" className="w-3.5 text-center text-[11px] font-semibold">
                {locale === 'zh' ? 'EN' : '中'}
              </span>
              <span className="truncate">
                {t('theme.locale')}: {locale === 'zh' ? '中文' : 'English'}
              </span>
            </Dropdown.Item>
            <Dropdown.Item id="logout" textValue={t('auth.logout')} className="text-danger">
              <LogOutIcon size={14} />
              <span className="truncate">{t('auth.logout')}</span>
            </Dropdown.Item>
          </Dropdown.Menu>
        </Dropdown.Popover>
      </Dropdown>
    </div>
  )
}

export default SidebarUserMenu
