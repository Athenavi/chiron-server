'use client'

/**
 * 成员管理页（仅 owner / admin）。
 *
 * - 非 owner/admin 直接展示无权限空态，不发任何请求；
 * - 列表分页（每页 20）：桌面语义表格 / 移动端卡片，操作统一走 Dropdown 菜单；
 * - 写操作：重置密码（确认 → 一次性密码弹窗）、禁用 / 启用、强制下线、
 *   删除（需逐字输入邮箱确认）；后端对「禁用自己 / 删除自己 / 最后一个 owner」
 *   的保护性错误直接 toast；
 * - 邀请成员：表单校验 + 角色 Select + 可选初始密码，成功后展示一次性密码并刷新；
 * - 加载骨架、空态、错误 Alert + 重试齐备，所有写操作都有 isPending 态；
 * - 全部界面文案走 i18n（`members.*`），邮箱 / 角色原值等技术标识保留原文。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import Link from 'next/link'
import { Alert, Button, EmptyState, toast } from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { PageHeader } from '@/components/shell/PageHeader'
import { ConfirmActionModal } from '@/components/members/ConfirmActionModal'
import { InviteMemberModal } from '@/components/members/InviteMemberModal'
import { MemberList, MemberListSkeleton } from '@/components/members/MemberList'
import { TempPasswordModal } from '@/components/members/TempPasswordModal'
import { isAdminRole, memberName } from '@/components/members/memberMeta'
import type { MemberAction } from '@/components/members/MemberActionsMenu'
import {
  AlertIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  LockIcon,
  MessageIcon,
  PlusIcon,
  RefreshIcon,
  UserIcon,
} from '@/components/ui-bridge/icons'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type CreateUserResult, type User } from '@/lib/types'

/** 与后端默认一致：每页 20 名成员。 */
const PER_PAGE = 20

interface ConfirmState {
  kind: 'reset-password' | 'delete'
  user: User
}

interface PendingState {
  id: string
  kind: MemberAction
}

interface SecretState {
  title: string
  email: string
  password: string
  description: string
}

export default function MembersPage() {
  const { t } = useI18n()
  const { user, plan } = useAuth()
  const isAdmin = isAdminRole(user?.role)

  const [page, setPage] = useState(1)
  const [users, setUsers] = useState<User[]>([])
  const [total, setTotal] = useState(0)
  const [perPage, setPerPage] = useState(PER_PAGE)
  const [loading, setLoading] = useState(true)
  const [refreshing, setRefreshing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState<PendingState | null>(null)
  const [confirm, setConfirm] = useState<ConfirmState | null>(null)
  const [secret, setSecret] = useState<SecretState | null>(null)
  const [inviteOpen, setInviteOpen] = useState(false)

  /** 请求序号：慢响应回来时丢弃，避免翻页竞态。 */
  const requestSeq = useRef(0)

  const load = useCallback(
    async (options: { silent?: boolean } = {}) => {
      const seq = (requestSeq.current += 1)
      if (options.silent) setRefreshing(true)
      else setLoading(true)
      setError(null)
      try {
        const result = await api.listUsers({ page, per_page: PER_PAGE })
        if (seq !== requestSeq.current) return
        const resolvedPerPage = result.per_page > 0 ? result.per_page : PER_PAGE
        const lastPage = Math.max(1, Math.ceil(result.total / resolvedPerPage))
        // 删除最后一页的最后一名成员后页码越界：回退一页（由 effect 重新拉取）
        if (page > lastPage) {
          setPage(lastPage)
          return
        }
        setUsers(result.users)
        setTotal(result.total)
        setPerPage(resolvedPerPage)
      } catch (cause) {
        if (seq !== requestSeq.current) return
        setError(ApiError.from(cause, t('members.error.load')).message)
      } finally {
        if (seq === requestSeq.current) {
          setLoading(false)
          setRefreshing(false)
        }
      }
    },
    [page, t],
  )

  useEffect(() => {
    if (!isAdmin) return
    void load()
  }, [isAdmin, load])

  // ── 写操作 ────────────────────────────────────────────────────────────

  async function runResetPassword(target: User) {
    setPending({ id: target.id, kind: 'reset-password' })
    try {
      const result = await api.resetUserPassword(target.id)
      setSecret({
        title: t('members.secret.resetTitle'),
        email: target.email,
        password: result.temporary_password,
        description: t('members.secret.resetDescription', { name: memberName(target) }),
      })
      toast.success(t('members.toast.secretGenerated'))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('members.error.resetPassword')).message)
    } finally {
      setPending(null)
      setConfirm(null)
    }
  }

  async function runDeleteUser(target: User) {
    setPending({ id: target.id, kind: 'delete' })
    try {
      await api.deleteUser(target.id)
      toast.success(t('members.toast.deleted', { name: memberName(target) }))
      setConfirm(null)
      if (users.length === 1 && page > 1) setPage(page - 1)
      else await load({ silent: true })
    } catch (cause) {
      // 后端保护（例如最后一个 owner）只把 message 抛给用户即可
      toast.danger(ApiError.from(cause, t('members.error.delete')).message)
      setConfirm(null)
    } finally {
      setPending(null)
    }
  }

  async function runStatusChange(target: User, next: 'active' | 'disabled') {
    setPending({ id: target.id, kind: next === 'disabled' ? 'disable' : 'enable' })
    try {
      if (next === 'disabled') await api.disableUser(target.id)
      else await api.enableUser(target.id)
      toast.success(
        next === 'disabled'
          ? t('members.toast.disabled', { name: memberName(target) })
          : t('members.toast.enabled', { name: memberName(target) }),
      )
      await load({ silent: true })
    } catch (cause) {
      toast.danger(
        ApiError.from(
          cause,
          next === 'disabled' ? t('members.error.disable') : t('members.error.enable'),
        ).message,
      )
    } finally {
      setPending(null)
    }
  }

  async function runForceLogout(target: User) {
    setPending({ id: target.id, kind: 'force-logout' })
    try {
      await api.forceLogout(target.id)
      toast.success(t('members.toast.forcedLogout', { name: memberName(target) }))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('members.error.forceLogout')).message)
    } finally {
      setPending(null)
    }
  }

  function handleAction(target: User, action: MemberAction) {
    if (pending) return
    switch (action) {
      case 'reset-password':
        setConfirm({ kind: 'reset-password', user: target })
        return
      case 'delete':
        setConfirm({ kind: 'delete', user: target })
        return
      case 'disable':
        void runStatusChange(target, 'disabled')
        return
      case 'enable':
        void runStatusChange(target, 'active')
        return
      case 'force-logout':
        void runForceLogout(target)
        return
      default:
        return
    }
  }

  function handleInvited(result: CreateUserResult) {
    setInviteOpen(false)
    if (result.initial_password) {
      setSecret({
        title: t('members.secret.createdTitle'),
        email: result.user.email,
        password: result.initial_password,
        description: t('members.secret.createdDescription', { name: memberName(result.user) }),
      })
    }
    void load({ silent: true })
  }

  // ── 无权限 ────────────────────────────────────────────────────────────

  if (!isAdmin) {
    return (
      <div>
        <PageHeader title={t('members.title')} description={t('members.description')} />
        <div className="content-shell py-6 md:py-8">
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <LockIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('members.noAccess.title')}</h2>
            <p className="max-w-md text-xs leading-5 text-muted">
              {t('members.noAccess.description')}
            </p>
            <Link
              href="/sessions"
              className="mt-1 inline-flex h-10 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary-hover"
            >
              <MessageIcon size={15} aria-hidden="true" />
              {t('members.actions.backToChat')}
            </Link>
          </EmptyState>
        </div>
      </div>
    )
  }

  // ── 有权限 ────────────────────────────────────────────────────────────

  const totalPages = Math.max(1, Math.ceil(total / perPage))
  const quotaHint = plan
    ? t('members.quota', { count: plan.max_users })
    : t('members.quotaFallback')
  const description =
    total > 0 ? t('members.descriptionTotal', { total, hint: quotaHint }) : quotaHint
  const confirmPending = pending !== null
  const confirmTitle =
    confirm?.kind === 'delete' ? t('members.confirm.deleteTitle') : t('members.confirm.resetTitle')
  const confirmDescription =
    confirm?.kind === 'delete'
      ? t('members.confirm.deleteDescription', { email: confirm.user.email })
      : confirm
        ? t('members.confirm.resetDescription', { email: confirm.user.email })
        : ''
  const confirmLabel =
    confirm?.kind === 'delete'
      ? t('members.confirm.deleteConfirm')
      : t('members.confirm.resetConfirm')

  return (
    <div>
      <PageHeader
        title={t('members.title')}
        description={description}
        actions={
          <>
            <Button
              variant="secondary"
              size="sm"
              className="h-10 gap-1.5 md:h-9"
              isPending={refreshing}
              isDisabled={loading}
              onPress={() => void load({ silent: true })}
            >
              {refreshing ? null : <RefreshIcon size={14} aria-hidden="true" />}
              {t('members.actions.refresh')}
            </Button>
            <Button
              variant="primary"
              size="sm"
              className="h-10 gap-1.5 md:h-9"
              onPress={() => setInviteOpen(true)}
            >
              <PlusIcon size={15} aria-hidden="true" />
              {t('members.actions.invite')}
            </Button>
          </>
        }
      />

      <div className="content-shell py-6 md:py-8">
        {error ? (
          <Alert status="danger" className="mb-4">
            <Alert.Indicator>
              <AlertIcon aria-hidden="true" className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>{t('members.error.loadTitle')}</Alert.Title>
              <Alert.Description>{error}</Alert.Description>
            </Alert.Content>
            <Button
              variant="secondary"
              size="sm"
              className="h-10 shrink-0 gap-1.5 self-center md:h-8"
              onPress={() => void load()}
            >
              <RefreshIcon size={14} aria-hidden="true" />
              {t('members.actions.retry')}
            </Button>
          </Alert>
        ) : null}

        {loading ? <MemberListSkeleton /> : null}

        {!loading && !error && users.length === 0 ? (
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <UserIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('members.empty.title')}</h2>
            <p className="max-w-sm text-xs leading-5 text-muted">{t('members.empty.description')}</p>
            <Button
              variant="primary"
              className="mt-1 h-10 gap-1.5"
              onPress={() => setInviteOpen(true)}
            >
              <PlusIcon size={15} aria-hidden="true" />
              {t('members.actions.invite')}
            </Button>
          </EmptyState>
        ) : null}

        {!loading && !error && users.length > 0 ? (
          <>
            <MemberList
              users={users}
              currentUserId={user?.id ?? null}
              busyId={pending?.id ?? null}
              actionsDisabled={pending !== null}
              onAction={handleAction}
            />

            <nav
              aria-label={t('members.pagination.aria')}
              className="mt-3 flex flex-wrap items-center justify-between gap-3"
            >
              <p className="text-xs text-muted" aria-live="polite">
                {t('members.pagination.summary', {
                  page,
                  totalPages,
                  total,
                  perPage,
                })}
              </p>
              <div className="flex items-center gap-2">
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-10 gap-1 md:h-8"
                  isDisabled={page <= 1 || refreshing}
                  onPress={() => setPage((current) => Math.max(1, current - 1))}
                >
                  <ChevronLeftIcon size={14} aria-hidden="true" />
                  {t('members.pagination.prev')}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-10 gap-1 md:h-8"
                  isDisabled={page >= totalPages || refreshing}
                  onPress={() => setPage((current) => Math.min(totalPages, current + 1))}
                >
                  {t('members.pagination.next')}
                  <ChevronRightIcon size={14} aria-hidden="true" />
                </Button>
              </div>
            </nav>
          </>
        ) : null}
      </div>

      <InviteMemberModal
        isOpen={inviteOpen}
        canInviteOwner={(user?.role ?? '').toLowerCase() === 'owner'}
        onOpenChange={setInviteOpen}
        onInvited={handleInvited}
      />

      <ConfirmActionModal
        isOpen={confirm !== null}
        title={confirmTitle}
        description={confirmDescription}
        confirmLabel={confirmLabel}
        danger={confirm?.kind === 'delete'}
        confirmationText={confirm?.kind === 'delete' ? confirm.user.email : undefined}
        pending={confirmPending}
        onConfirm={() => {
          if (!confirm) return
          if (confirm.kind === 'delete') void runDeleteUser(confirm.user)
          else void runResetPassword(confirm.user)
        }}
        onClose={() => {
          if (!confirmPending) setConfirm(null)
        }}
      />

      <TempPasswordModal
        isOpen={secret !== null}
        title={secret?.title ?? ''}
        email={secret?.email ?? ''}
        password={secret?.password ?? ''}
        description={secret?.description ?? ''}
        onClose={() => setSecret(null)}
      />
    </div>
  )
}
