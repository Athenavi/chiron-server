'use client'

/**
 * 成员管理页共用的展示原子：角色 / 状态徽标、头像与名称兜底、时间格式化。
 *
 * 只做呈现与文案映射，不发请求；页面与列表组件都从这里取，保证同一角色在
 * 桌面表格和移动端卡片里的颜色、文案完全一致。文案走 i18n（`members.*`），
 * 未知角色 / 状态回落为后端原值，避免吞掉技术标识。
 */

import { Avatar, Chip } from '@heroui/react'
import type { User } from '@/lib/types'
import { initials } from '@/lib/utils'
import { useI18n, type Locale } from '@/lib/i18n'

/** `useI18n().t` 的签名，便于在非 hook 模块里复用文案。 */
export type Translator = (key: string, vars?: Record<string, string | number>) => string

const KNOWN_ROLES = ['owner', 'admin', 'user'] as const

/** 角色文案（未知角色回落到后端原值）。 */
export function roleLabel(role: string | null | undefined, t: Translator): string {
  const key = (role ?? '').trim().toLowerCase()
  if ((KNOWN_ROLES as readonly string[]).includes(key)) return t(`members.role.${key}`)
  return (role ?? '').trim() || t('members.role.unknown')
}

export function isAdminRole(role: string | null | undefined): boolean {
  const key = (role ?? '').trim().toLowerCase()
  return key === 'owner' || key === 'admin'
}

/** 展示名：昵称 → 显示名 → 用户名 → 邮箱。 */
export function memberName(
  user: Pick<User, 'nickname' | 'name' | 'username' | 'email'>,
): string {
  return user.nickname?.trim() || user.name?.trim() || user.username?.trim() || user.email
}

/** 日期时间：`11/21 09:30`（按 locale 输出，读屏与 title 提示共用）。 */
export function formatTimestamp(value: string | null | undefined, locale: Locale): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleString(locale === 'en' ? 'en-US' : 'zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

/** 最近登录：绝对时间，从未登录时给出显式文案。 */
export function lastLoginLabel(value: string | null, locale: Locale, t: Translator): string {
  return value ? formatTimestamp(value, locale) : t('members.table.never')
}

/** 角色徽标：owner 强调色、admin 警告色、user 中性色。 */
export function RoleChip({ role }: { role: string }) {
  const { t } = useI18n()
  const key = (role ?? '').trim().toLowerCase()
  const color = key === 'owner' ? 'accent' : key === 'admin' ? 'warning' : 'default'
  return (
    <Chip size="sm" variant="soft" color={color}>
      {roleLabel(role, t)}
    </Chip>
  )
}

/** 状态徽标：active 绿、其余红；未知状态保留后端原值。 */
export function StatusChip({ status }: { status: string }) {
  const { t } = useI18n()
  const key = (status ?? '').trim().toLowerCase()
  const active = key === 'active'
  const label =
    key === 'active'
      ? t('members.status.active')
      : key === 'disabled'
        ? t('members.status.disabled')
        : key === 'deleted'
          ? t('members.status.deleted')
          : key || t('members.status.unknown')
  return (
    <Chip size="sm" variant="soft" color={active ? 'success' : 'danger'}>
      {label}
    </Chip>
  )
}

/** 成员头像（无头像图时用缩写兜底）。 */
export function MemberAvatar({
  user,
  size = 'sm',
}: {
  user: Pick<User, 'nickname' | 'name' | 'username' | 'email'>
  size?: 'sm' | 'md' | 'lg'
}) {
  const name = memberName(user)
  return (
    <Avatar size={size} color="accent" aria-label={name} className="shrink-0">
      <Avatar.Fallback>{initials(name)}</Avatar.Fallback>
    </Avatar>
  )
}
