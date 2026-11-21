'use client'

/**
 * 设置页：账户资料 + 套餐信息 + 外观/语言 + 退出登录。
 *
 * 轻量实现，不做成员管理：
 * - 昵称 / 显示名可编辑，保存调用 `api.updateProfile` 后 `refresh()` 同步侧栏；
 * - 套餐字段只读展示（来自 `useAuth().plan`）；
 * - 外观使用 `ThemeSwitcher`，语言使用 `LocaleSwitcher`（仅存本机）；
 * - 退出登录后回 `/login`。
 */

import { useEffect, useState } from 'react'
import Link from 'next/link'
import { useRouter } from 'next/navigation'
import {
  Avatar,
  Button,
  Card,
  Chip,
  Description,
  Input,
  Label,
  TextField,
  toast,
} from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { LocaleSwitcher } from '@/components/theme/LocaleSwitcher'
import { ThemeSwitcher } from '@/components/theme/ThemeSwitcher'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError } from '@/lib/types'
import { LogOutIcon, LockIcon } from '@/components/ui-bridge/icons'

type Translate = (key: string, vars?: Record<string, string | number>) => string

const KNOWN_ROLES = ['owner', 'admin', 'member', 'viewer'] as const
type KnownRole = (typeof KNOWN_ROLES)[number]

function isKnownRole(role: string | null | undefined): role is KnownRole {
  return Boolean(role) && (KNOWN_ROLES as readonly string[]).includes(role as string)
}

function roleLabel(t: Translate, role: string | null | undefined): string {
  return isKnownRole(role) ? t(`settings.roles.${role}`) : (role ?? t('settings.roles.member'))
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-border py-3 last:border-b-0">
      <dt className="shrink-0 text-[13px] text-muted">{label}</dt>
      <dd className="min-w-0 truncate text-right text-[13px] font-medium text-foreground">
        {children}
      </dd>
    </div>
  )
}

export default function SettingsPage() {
  const router = useRouter()
  const { locale, t } = useI18n()
  const { user, plan, refresh, logout } = useAuth()

  const [nickname, setNickname] = useState(user?.nickname ?? '')
  const [name, setName] = useState(user?.name ?? '')
  const [nameError, setNameError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [loggingOut, setLoggingOut] = useState(false)

  // 保存 / 刷新用户信息后把表单同步到最新值
  useEffect(() => {
    setNickname(user?.nickname ?? '')
    setName(user?.name ?? '')
    setNameError(null)
  }, [user?.id, user?.nickname, user?.name])

  const dateLocale = locale === 'en' ? 'en-US' : 'zh-CN'
  const displayName =
    user?.nickname?.trim() || user?.name || user?.email || t('settings.currentUser')
  const dirty = nickname !== (user?.nickname ?? '') || name !== (user?.name ?? '')

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (saving || !dirty) return
    if (!name.trim()) {
      setNameError(t('settings.nameRequired'))
      return
    }
    setNameError(null)
    setSaving(true)
    try {
      await api.updateProfile({ nickname: nickname.trim(), name: name.trim() })
      await refresh()
      toast.success(t('settings.saved'))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('settings.saveFailed')).message)
    } finally {
      setSaving(false)
    }
  }

  async function handleLogout() {
    if (loggingOut) return
    setLoggingOut(true)
    try {
      await logout()
    } catch {
      // 退出接口失败也要回登录页：本地登录态已清理
    } finally {
      setLoggingOut(false)
      router.replace('/login')
    }
  }

  return (
    <div className="content-shell max-w-3xl py-6 md:py-8">
      <header>
        <h1 className="text-lg font-semibold tracking-tight text-foreground">
          {t('settings.title')}
        </h1>
        <p className="mt-1 text-sm text-muted">{t('settings.subtitle')}</p>
      </header>

      {/* 账户资料 */}
      <Card className="mt-6">
        <Card.Header className="flex-row items-center gap-3">
          <Avatar size="md" color="accent" aria-label={displayName}>
            <Avatar.Fallback>{(displayName.trim() || '?').slice(0, 1)}</Avatar.Fallback>
          </Avatar>
          <div className="min-w-0 flex-1">
            <Card.Title className="truncate">{displayName}</Card.Title>
            <Card.Description className="truncate">{user?.email ?? '—'}</Card.Description>
          </div>
          <Chip
            size="sm"
            color="accent"
            variant="soft"
            aria-label={`${t('settings.role')}: ${roleLabel(t, user?.role)}`}
            className="shrink-0"
          >
            {roleLabel(t, user?.role)}
          </Chip>
        </Card.Header>

        <Card.Content>
          <form onSubmit={handleSubmit} className="flex flex-col gap-4" noValidate>
            <div className="grid gap-4 sm:grid-cols-2">
              <TextField
                fullWidth
                isDisabled={saving}
                value={nickname}
                onChange={(value) => setNickname(value)}
              >
                <Label>{t('settings.nickname')}</Label>
                <Input
                  placeholder={t('settings.nicknamePlaceholder')}
                  autoComplete="nickname"
                />
                <Description>{t('settings.nicknameHint')}</Description>
              </TextField>

              <TextField
                fullWidth
                isDisabled={saving}
                isInvalid={Boolean(nameError)}
                value={name}
                onChange={(value) => setName(value)}
              >
                <Label>{t('settings.displayName')}</Label>
                <Input placeholder={t('settings.namePlaceholder')} autoComplete="name" />
                {nameError ? <Description className="text-danger">{nameError}</Description> : null}
              </TextField>
            </div>

            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="submit"
                variant="primary"
                isPending={saving}
                isDisabled={!dirty}
                className="h-10 md:h-9"
              >
                {t('settings.saveProfile')}
              </Button>
              <Link
                href="/change-password"
                className="inline-flex h-10 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-muted transition-colors hover:bg-surface-hover hover:text-foreground md:h-9"
              >
                <LockIcon size={14} aria-hidden="true" />
                {t('settings.changePassword')}
              </Link>
            </div>
          </form>

          <dl className="mt-4">
            <Row label={t('settings.username')}>
              {user?.username || t('settings.unset')}
            </Row>
            <Row label={t('settings.lastLogin')}>
              {user?.last_login_at
                ? new Date(user.last_login_at).toLocaleString(dateLocale)
                : '—'}
            </Row>
          </dl>
        </Card.Content>
      </Card>

      {/* 套餐信息 */}
      <Card className="mt-4">
        <Card.Header>
          <Card.Title>{t('settings.plan')}</Card.Title>
          <Card.Description>{t('settings.planHint')}</Card.Description>
        </Card.Header>
        <Card.Content>
          {plan ? (
            <dl className="grid gap-x-8 sm:grid-cols-2">
              <Row label={t('settings.planName')}>{plan.plan_id}</Row>
              <Row label={t('settings.concurrentRuns')}>{plan.max_concurrent_runs}</Row>
              <Row label={t('settings.monthlyTokens')}>
                {plan.monthly_tokens.toLocaleString(dateLocale)}
              </Row>
              <Row label={t('settings.maxUsers')}>{plan.max_users}</Row>
              <Row label={t('settings.storage')}>{plan.max_storage_gb} GB</Row>
              <Row label={t('settings.sandboxes')}>{plan.max_sandboxes}</Row>
            </dl>
          ) : (
            <p className="text-sm text-muted">{t('settings.planEmpty')}</p>
          )}
        </Card.Content>
      </Card>

      {/* 外观与语言 */}
      <Card className="mt-4">
        <Card.Header className="flex-col items-start gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="min-w-0">
            <Card.Title>{t('settings.appearance')}</Card.Title>
            <Card.Description>{t('settings.appearanceHint')}</Card.Description>
          </div>
          <ThemeSwitcher className="min-h-10 shrink-0 md:min-h-9" />
        </Card.Header>
        <Card.Content>
          <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-4">
            <div className="min-w-0">
              <p className="text-[13px] font-medium text-foreground">{t('settings.language')}</p>
              <p className="mt-0.5 text-xs text-muted">{t('settings.localeHint')}</p>
            </div>
            <LocaleSwitcher className="min-h-10 md:min-h-9" />
          </div>
        </Card.Content>
      </Card>

      {/* 退出登录 */}
      <Card className="mt-4">
        <Card.Header className="flex-row items-center justify-between gap-3">
          <div className="min-w-0">
            <Card.Title>{t('settings.logoutTitle')}</Card.Title>
            <Card.Description>{t('settings.logoutHint')}</Card.Description>
          </div>
          <Button
            variant="danger-soft"
            isPending={loggingOut}
            onPress={() => void handleLogout()}
            className="h-10 shrink-0 gap-1.5 md:h-9"
          >
            {loggingOut ? null : <LogOutIcon size={15} aria-hidden="true" />}
            {t('auth.logout')}
          </Button>
        </Card.Header>
      </Card>
    </div>
  )
}
