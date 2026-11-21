'use client'

/**
 * 成员列表：桌面端语义表格（caption + scope），移动端卡片列表。
 *
 * 两种布局共用同一份数据与操作菜单；表格不依赖 HeroUI Table 的
 * render-prop API，用原生 table 保证 `th[scope]` / `caption` 的语义完整，
 * 视觉仍沿用 HeroUI 的 surface / border / chip 令牌。
 *
 * 响应式：`md` 以上表格（table-fixed + 固定列宽，长邮箱省略不撑破布局），
 * `md` 以下卡片；360px 下邮箱 / 昵称单行省略，完整值放在 title 里。
 */

import { Chip, Spinner } from '@heroui/react'
import type { User } from '@/lib/types'
import { Skeleton } from '@/components/ui-bridge/skeleton'
import { useI18n } from '@/lib/i18n'
import { MemberActionsMenu, type MemberAction } from './MemberActionsMenu'
import {
  MemberAvatar,
  RoleChip,
  StatusChip,
  formatTimestamp,
  lastLoginLabel,
  memberName,
} from './memberMeta'

export interface MemberListProps {
  users: User[]
  /** 当前登录账号 id（用于「你」标记与菜单文案）。 */
  currentUserId: string | null
  /** 正在执行写操作的成员 id（该行显示 Spinner）。 */
  busyId: string | null
  /** 任一成员有写操作在飞：所有行菜单暂时禁用，避免并发操作互相覆盖。 */
  actionsDisabled?: boolean
  onAction: (user: User, action: MemberAction) => void
}

function SelfChip() {
  const { t } = useI18n()
  return (
    <Chip size="sm" variant="soft" color="accent" className="shrink-0">
      {t('members.table.self')}
    </Chip>
  )
}

export function MemberList({
  users,
  currentUserId,
  busyId,
  actionsDisabled = false,
  onAction,
}: MemberListProps) {
  const { t, locale } = useI18n()

  return (
    <>
      {/* 桌面：语义表格 */}
      <div className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block">
        <table className="w-full table-fixed border-collapse text-left text-sm">
          <caption className="sr-only">{t('members.table.caption')}</caption>
          <thead>
            <tr className="border-b border-border bg-surface-secondary/60 text-[11px] tracking-wide text-subtle">
              <th scope="col" className="px-4 py-2.5 font-medium">
                {t('members.table.member')}
              </th>
              <th scope="col" className="w-24 px-3 py-2.5 font-medium">
                {t('members.table.role')}
              </th>
              <th scope="col" className="w-24 px-3 py-2.5 font-medium">
                {t('members.table.status')}
              </th>
              <th scope="col" className="w-32 px-3 py-2.5 font-medium">
                {t('members.table.lastLogin')}
              </th>
              <th scope="col" className="w-14 px-2 py-2.5">
                <span className="sr-only">{t('members.table.actions')}</span>
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border">
            {users.map((user) => {
              const isSelf = user.id === currentUserId
              const busy = busyId === user.id
              return (
                <tr key={user.id} className="transition-colors hover:bg-surface-hover/60">
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-2.5">
                      <MemberAvatar user={user} />
                      <div className="min-w-0">
                        <div className="flex items-center gap-1.5">
                          <span className="truncate text-[13px] font-medium text-foreground">
                            {memberName(user)}
                          </span>
                          {isSelf ? <SelfChip /> : null}
                          {busy ? (
                            <Spinner
                              size="sm"
                              color="current"
                              aria-label={t('members.table.busy')}
                            />
                          ) : null}
                        </div>
                        <p className="truncate text-[11px] text-muted" title={user.email}>
                          {user.email}
                        </p>
                      </div>
                    </div>
                  </td>
                  <td className="px-3 py-3">
                    <RoleChip role={user.role} />
                  </td>
                  <td className="px-3 py-3">
                    <StatusChip status={user.status} />
                  </td>
                  <td className="px-3 py-3 text-[12px] text-muted">
                    <span
                      className="block truncate"
                      title={
                        user.last_login_at
                          ? formatTimestamp(user.last_login_at, locale)
                          : undefined
                      }
                    >
                      {lastLoginLabel(user.last_login_at, locale, t)}
                    </span>
                  </td>
                  <td className="px-2 py-3 text-right">
                    <MemberActionsMenu
                      user={user}
                      isSelf={isSelf}
                      isBusy={busy || actionsDisabled}
                      onAction={(action) => onAction(user, action)}
                    />
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      {/* 移动端：卡片列表 */}
      <ul className="flex flex-col gap-2 md:hidden" aria-label={t('members.table.mobileAria')}>
        {users.map((user) => {
          const isSelf = user.id === currentUserId
          const busy = busyId === user.id
          return (
            <li
              key={user.id}
              className="rounded-xl border border-border bg-surface p-3 shadow-xs"
            >
              <div className="flex items-start gap-3">
                <MemberAvatar user={user} size="md" />
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate text-[13px] font-medium text-foreground">
                      {memberName(user)}
                    </span>
                    {isSelf ? <SelfChip /> : null}
                    {busy ? (
                      <Spinner size="sm" color="current" aria-label={t('members.table.busy')} />
                    ) : null}
                  </div>
                  <p className="truncate text-[11px] text-muted" title={user.email}>
                    {user.email}
                  </p>
                </div>
                <MemberActionsMenu
                  user={user}
                  isSelf={isSelf}
                  isBusy={busy || actionsDisabled}
                  onAction={(action) => onAction(user, action)}
                />
              </div>
              <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
                <RoleChip role={user.role} />
                <StatusChip status={user.status} />
                <span className="text-[11px] text-subtle">
                  {t('members.table.lastLoginInline', {
                    time: lastLoginLabel(user.last_login_at, locale, t),
                  })}
                </span>
              </div>
            </li>
          )
        })}
      </ul>
    </>
  )
}

/** 加载骨架（桌面表格行 + 移动端卡片，保持与真实布局相同的节奏）。 */
export function MemberListSkeleton() {
  const { t } = useI18n()
  const rows = [0, 1, 2, 3, 4]
  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{t('members.skeleton.aria')}</span>

      <div
        aria-hidden="true"
        className="hidden overflow-hidden rounded-xl border border-border bg-surface shadow-xs md:block"
      >
        <div className="flex items-center gap-4 border-b border-border bg-surface-secondary/60 px-4 py-3">
          <Skeleton className="h-2.5 w-16" />
          <Skeleton className="h-2.5 w-12" />
          <Skeleton className="h-2.5 w-12" />
          <Skeleton className="h-2.5 w-20" />
        </div>
        <ul className="divide-y divide-border">
          {rows.map((row) => (
            <li key={row} className="flex items-center gap-3 px-4 py-3.5">
              <Skeleton className="size-8 shrink-0 rounded-full" />
              <div className="min-w-0 flex-1">
                <Skeleton className="h-3" style={{ width: `${38 + row * 7}%` }} />
                <Skeleton className="mt-2 h-2.5 w-36" />
              </div>
              <Skeleton className="h-5 w-12 shrink-0 rounded-full" />
              <Skeleton className="h-5 w-12 shrink-0 rounded-full" />
              <Skeleton className="h-3 w-20 shrink-0" />
            </li>
          ))}
        </ul>
      </div>

      <ul className="flex flex-col gap-2 md:hidden" aria-hidden="true">
        {rows.map((row) => (
          <li key={row} className="rounded-xl border border-border bg-surface p-3 shadow-xs">
            <div className="flex items-center gap-3">
              <Skeleton className="size-9 shrink-0 rounded-full" />
              <div className="min-w-0 flex-1">
                <Skeleton className="h-3" style={{ width: `${42 + row * 8}%` }} />
                <Skeleton className="mt-2 h-2.5 w-32" />
              </div>
            </div>
            <div className="mt-3 flex gap-2">
              <Skeleton className="h-5 w-12 rounded-full" />
              <Skeleton className="h-5 w-12 rounded-full" />
            </div>
          </li>
        ))}
      </ul>
    </div>
  )
}
