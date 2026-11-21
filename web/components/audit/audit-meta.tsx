'use client'

/**
 * 审计页共用的展示原子：动作文案 / Chip 颜色、用户展示兜底、时间格式化。
 *
 * 只做映射，不发请求；表格与移动端卡片共用，保证同一动作在两处颜色一致。
 * 颜色规则（来自 PRD）：登录 = success、密码/用户变更 = warning、
 * 集成/市场 = accent、其余 = default。
 * 动作文案走 `audit.list.actions.*` i18n；未收录的动作码保留后端原值（技术标识）。
 */

import { Chip } from '@heroui/react'
import { useI18n, type Locale } from '@/lib/i18n'

export type AuditChipColor = 'success' | 'warning' | 'accent' | 'default'

/** 动作码 → i18n 键。键名不含点号，避免与 `t()` 的路径分隔冲突。 */
const ACTION_KEYS: Record<string, string> = {
  login_success: 'audit.list.actions.loginSuccess',
  login_failed: 'audit.list.actions.loginFailed',
  login_failure: 'audit.list.actions.loginFailed',
  logout: 'audit.list.actions.logout',
  password_changed: 'audit.list.actions.passwordChanged',
  password_reset: 'audit.list.actions.passwordReset',
  user_created: 'audit.list.actions.userCreated',
  user_updated: 'audit.list.actions.userUpdated',
  user_disabled: 'audit.list.actions.userDisabled',
  user_enabled: 'audit.list.actions.userEnabled',
  user_deleted: 'audit.list.actions.userDeleted',
  force_logout: 'audit.list.actions.forceLogout',
  'mcp.server.probe': 'audit.list.actions.mcpProbe',
  'mcp.server.create': 'audit.list.actions.mcpCreate',
  'mcp.server.delete': 'audit.list.actions.mcpDelete',
  'market.install': 'audit.list.actions.marketInstall',
  'market.uninstall': 'audit.list.actions.marketUninstall',
  session_created: 'audit.list.actions.sessionCreated',
  session_deleted: 'audit.list.actions.sessionDeleted',
}

export function auditActionLabel(
  action: string,
  t: (key: string, vars?: Record<string, string | number>) => string,
): string {
  if (!action) return t('audit.list.unknownAction')
  const key = ACTION_KEYS[action]
  /** 未收录的动作码是技术标识，直接显示后端原值。 */
  return key ? t(key) : action
}

export function auditActionColor(action: string): AuditChipColor {
  const key = (action ?? '').toLowerCase()
  if (key.includes('login') || key.includes('logout') || key.includes('auth')) return 'success'
  if (
    key.includes('password') ||
    key.includes('user') ||
    key.includes('member') ||
    key.includes('role')
  ) {
    return 'warning'
  }
  if (
    key.includes('mcp') ||
    key.includes('market') ||
    key.includes('plugin') ||
    key.includes('skill') ||
    key.includes('integration')
  ) {
    return 'accent'
  }
  return 'default'
}

export function AuditActionChip({ action }: { action: string }) {
  const { t } = useI18n()

  return (
    <Chip
      size="sm"
      variant="soft"
      color={auditActionColor(action)}
      className="max-w-full truncate"
      title={action}
    >
      {auditActionLabel(action, t)}
    </Chip>
  )
}

/** `user_id` 可能为空（系统任务 / 匿名请求）。 */
export function auditUserLabel(
  userId: string | null | undefined,
  t: (key: string) => string,
): string {
  const trimmed = (userId ?? '').trim()
  return trimmed || t('audit.list.systemUser')
}

function intlTag(locale: Locale): string {
  return locale === 'en' ? 'en-US' : 'zh-CN'
}

/** 列表展示：本地时区 `MM-DD HH:mm`。 */
export function auditTimestamp(value: string | null | undefined, locale: Locale): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return new Intl.DateTimeFormat(intlTag(locale), {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  }).format(date)
}

/** `title` 悬浮提示：完整日期 + 秒。 */
export function auditAbsolute(value: string | null | undefined, locale: Locale): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return new Intl.DateTimeFormat(intlTag(locale), {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(date)
}

/** 详情超过这个长度才给「展开」入口，避免短详情也出现按钮。 */
export const DETAIL_PREVIEW_LENGTH = 56

export function auditDetailIsLong(detail: string): boolean {
  return (detail ?? '').trim().length > DETAIL_PREVIEW_LENGTH
}
