'use client'

/**
 * 会话侧栏（数据层）。
 *
 * - 挂载时拉一次 `api.listSessions({ status: 'active', limit: 50 })`，路由变化后再静默刷新；
 * - 搜索框在本地按标题 / 模型过滤，列表按更新时间分组「今天 / 昨天 / 更早」；
 * - 新建会话成功后直接跳到 `/chat/{id}`；
 * - 归档 / 删除 / 重命名都走 `api`，成功后局部更新列表；删除的是当前会话时跳 `/sessions`；
 * - 底部固定用户区（头像 + 账户菜单）。
 *
 * 侧栏本体是深色的，配色一律用 `--app-sidebar-*` 令牌（`bg-sidebar` 等），
 * 不随内容区主题翻转。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { usePathname, useRouter } from 'next/navigation'
import { Button, SearchField, Skeleton, toast } from '@heroui/react'
import { api } from '@/lib/api'
import { exportSession, shareSession } from '@/lib/session-export'
import { ApiError, type ChatSession } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import type { SessionItemProps } from '@/components/sidebar/SessionItem'
import { SessionItem } from '@/components/sidebar/SessionItem'
import { SidebarUserMenu } from '@/components/sidebar/SidebarUserMenu'
import { PlusIcon, RefreshIcon } from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

type GroupKey = 'today' | 'yesterday' | 'earlier'

interface SessionGroup {
  key: GroupKey
  items: ChatSession[]
}

const DAY_MS = 86_400_000

function startOfDay(value: number): number {
  const date = new Date(value)
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime()
}

function groupSessions(sessions: ChatSession[]): SessionGroup[] {
  const today = startOfDay(Date.now())
  const yesterday = today - DAY_MS
  const buckets: Record<GroupKey, ChatSession[]> = { today: [], yesterday: [], earlier: [] }

  const sorted = [...sessions].sort((a, b) => b.updated_at.localeCompare(a.updated_at))
  for (const session of sorted) {
    const time = new Date(session.updated_at).getTime()
    if (Number.isNaN(time)) buckets.earlier.push(session)
    else if (time >= today) buckets.today.push(session)
    else if (time >= yesterday) buckets.yesterday.push(session)
    else buckets.earlier.push(session)
  }

  // 分组标题在渲染时按 `chat.sidebar.groups.*` 取文案，避免把中文写死在这里
  const groups: SessionGroup[] = [
    { key: 'today', items: buckets.today },
    { key: 'yesterday', items: buckets.yesterday },
    { key: 'earlier', items: buckets.earlier },
  ]
  return groups.filter((group) => group.items.length > 0)
}

/** 从 `/chat/{id}` 里取当前会话 id（父级没显式传 activeSessionId 时兜底）。 */
function activeIdFromPath(pathname: string | null): string | undefined {
  const match = pathname ? /^\/chat\/([^/?#]+)/.exec(pathname) : null
  return match ? match[1] : undefined
}

export function SessionSidebar({ activeSessionId }: { activeSessionId?: string }) {
  const router = useRouter()
  const { t } = useI18n()
  const pathname = usePathname()
  const activeId = activeSessionId ?? activeIdFromPath(pathname)

  const [sessions, setSessions] = useState<ChatSession[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [creating, setCreating] = useState(false)
  const [busyId, setBusyId] = useState<string | null>(null)
  const [pendingTotal, setPendingTotal] = useState(0)
  const mountedRef = useRef(true)

  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const load = useCallback(async (quiet = false) => {
    if (!quiet) setLoading(true)
    if (!quiet) setError(null)
    try {
      const result = await api.listSessions({ status: 'active', limit: 50 })
      if (!mountedRef.current) return
      setSessions(result.sessions)
      setPendingTotal(
        result.pending_approvals_total ??
          result.sessions.reduce((sum, session) => sum + (session.pending_approvals ?? 0), 0),
      )
      setError(null)
    } catch (cause) {
      if (!mountedRef.current) return
      // 静默刷新失败时保留已有列表，只在首次加载时展示错误
      if (!quiet) setError(ApiError.from(cause, t('chat.sidebar.loadFailed')).message)
    } finally {
      if (mountedRef.current) setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load])

  // 待审批可能在别的会话里出现/消失：页面可见时每 20 秒静默刷新一次。
  useEffect(() => {
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void load(true)
    }, 20_000)
    return () => window.clearInterval(timer)
  }, [load])

  // 路由变化（新建会话、从列表跳转）后静默刷新，保证侧栏与 URL 一致
  const firstRouteRef = useRef(true)
  useEffect(() => {
    if (firstRouteRef.current) {
      firstRouteRef.current = false
      return
    }
    void load(true)
  }, [pathname, load])

  const filtered = useMemo(() => {
    const keyword = query.trim().toLowerCase()
    if (!keyword) return sessions
    return sessions.filter((session) => {
      const title = session.title?.toLowerCase() ?? ''
      const model = session.model?.toLowerCase() ?? ''
      return title.includes(keyword) || model.includes(keyword)
    })
  }, [sessions, query])

  const groups = useMemo(() => groupSessions(filtered), [filtered])

  async function handleCreate() {
    if (creating) return
    setCreating(true)
    try {
      const { session } = await api.createSession({})
      setSessions((previous) => {
        if (previous.some((item) => item.id === session.id)) return previous
        return [session, ...previous]
      })
      router.push(`/chat/${session.id}`)
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sidebar.createFailed')).message)
    } finally {
      setCreating(false)
    }
  }

  const handleRename: SessionItemProps['onRename'] = async (session, title) => {
    try {
      const { session: updated } = await api.updateSession(session.id, { title })
      setSessions((previous) =>
        previous.map((item) => (item.id === updated.id ? updated : item)),
      )
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sidebar.renameFailed')).message)
      throw cause
    }
  }

  const handleArchive: SessionItemProps['onArchive'] = (session) => {
    if (busyId) return
    setBusyId(session.id)
    void (async () => {
      try {
        await api.updateSession(session.id, { archived: true })
        if (!mountedRef.current) return
        setSessions((previous) => previous.filter((item) => item.id !== session.id))
        toast.success(t('chat.sidebar.archived'))
        if (session.id === activeId) router.push('/sessions')
      } catch (cause) {
        toast.danger(ApiError.from(cause, t('chat.sidebar.archiveFailed')).message)
      } finally {
        if (mountedRef.current) setBusyId(null)
      }
    })()
  }

  const handleExport: SessionItemProps['onExport'] = (session) => {
    if (busyId) return
    setBusyId(session.id)
    void (async () => {
      try {
        const count = await exportSession(session)
        toast.success(t('chat.sessionActions.exported', { n: count }))
      } catch (cause) {
        toast.danger(ApiError.from(cause, t('chat.sessionActions.exportFailed')).message)
      } finally {
        if (mountedRef.current) setBusyId(null)
      }
    })()
  }

  const handleShare: SessionItemProps['onShare'] = (session) => {
    void (async () => {
      try {
        const result = await shareSession(session)
        if (result === 'shared') toast.success(t('chat.sessionActions.shareSuccess'))
        else if (result === 'copied') toast.success(t('chat.sessionActions.shareCopied'))
      } catch (cause) {
        toast.danger(ApiError.from(cause, t('chat.sessionActions.shareFailed')).message)
      }
    })()
  }

  const handleDelete: SessionItemProps['onDelete'] = async (session) => {
    setBusyId(session.id)
    try {
      await api.deleteSession(session.id)
      if (!mountedRef.current) return
      setSessions((previous) => previous.filter((item) => item.id !== session.id))
      toast.success(t('chat.sidebar.deleted'))
      if (session.id === activeId) router.push('/sessions')
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sidebar.deleteFailed')).message)
      throw cause
    } finally {
      if (mountedRef.current) setBusyId(null)
    }
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* 新建会话（桌面折叠后只剩一个 44px 的 + 图标） */}
      <div className="shrink-0 px-3 pb-2">
        <Button
          variant="ghost"
          fullWidth
          isPending={creating}
          aria-label={t('chat.sidebar.newSession')}
          onPress={() => void handleCreate()}
          className={cn(
            'sidebar-icon-button h-10 justify-start gap-2 rounded-md border border-sidebar-border bg-sidebar-elevated px-3 md:h-9',
            'text-[13px] font-medium text-sidebar-foreground shadow-none',
            'hover:bg-sidebar-hover',
          )}
        >
          {creating ? null : <PlusIcon size={15} className="text-sidebar-muted" />}
          <span className="sidebar-expanded-only">{t('chat.sidebar.newSession')}</span>
        </Button>
      </div>

      {/* 搜索（折叠后整体隐藏） */}
      <div className="sidebar-expanded-only shrink-0 px-3 pb-2">
        <SearchField
          aria-label={t('chat.sidebar.searchAria')}
          fullWidth
          value={query}
          onChange={setQuery}
          className="w-full"
        >
          <SearchField.Group className="h-10 w-full border-sidebar-border bg-sidebar-elevated text-sidebar-foreground shadow-none md:h-8">
            <SearchField.SearchIcon className="ms-2 size-3.5 shrink-0 text-sidebar-muted" />
            <SearchField.Input
              placeholder={t('chat.sidebar.searchPlaceholder')}
              className="px-2 py-0 text-[13px] text-sidebar-foreground placeholder:text-sidebar-muted"
            />
            <SearchField.ClearButton
              aria-label={t('chat.sidebar.clearSearch')}
              className="me-1 size-6 text-sidebar-muted"
            />
          </SearchField.Group>
        </SearchField>
      </div>

      {/* 列表（折叠后整体隐藏，只留导航图标 rail） */}
      <div className="sidebar-expanded-only scrollbar-sidebar flex min-h-0 flex-1 flex-col overflow-y-auto px-2 pb-2">
        <div className="flex items-center justify-between px-2 pb-1 pt-1">
          <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-sidebar-muted">
            {t('chat.sidebar.heading')}
          </span>
          <span className="flex items-center gap-1.5 text-[11px] tabular-nums text-sidebar-muted">
            {pendingTotal > 0 ? (
              <button
                type="button"
                title={t('chat.sidebar.pendingAria', { n: pendingTotal })}
                aria-label={t('chat.sidebar.pendingAria', { n: pendingTotal })}
                onClick={() => {
                  const target = sessions.find((session) => (session.pending_approvals ?? 0) > 0)
                  if (target) router.push(`/chat/${target.id}`)
                }}
                className="rounded-full bg-warning/20 px-1.5 py-0.5 text-[10px] font-semibold text-warning transition-colors hover:bg-warning/30"
              >
                {pendingTotal}
              </button>
            ) : null}
            {sessions.length > 0
              ? t('chat.sidebar.count', { n: sessions.length })
              : t('chat.sidebar.recent')}
          </span>
        </div>

        {loading ? (
          <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-1.5">
            <span className="sr-only">{t('chat.sidebar.loading')}</span>
            {Array.from({ length: 7 }, (_, index) => (
              <div
                key={index}
                className="flex items-center gap-2.5 rounded-md px-2 py-2.5"
                aria-hidden="true"
              >
                <Skeleton className="size-3.5 shrink-0 rounded-sm bg-white/10" />
                <Skeleton
                  className="h-3 rounded-sm bg-white/10"
                  style={{ width: `${56 + ((index * 13) % 36)}%` }}
                />
              </div>
            ))}
          </div>
        ) : error ? (
          <div className="flex flex-col items-start gap-2 px-2 py-4">
            <p className="text-[12px] leading-5 text-sidebar-muted">{error}</p>
            <Button
              variant="ghost"
              size="sm"
              className="h-10 gap-1.5 rounded-md border border-sidebar-border px-2.5 text-[12px] text-sidebar-foreground hover:bg-sidebar-hover md:h-8"
              onPress={() => void load()}
            >
              <RefreshIcon size={13} />
              {t('common.retry')}
            </Button>
          </div>
        ) : groups.length === 0 ? (
          <div className="px-2 py-6 text-center">
            <p className="text-[12px] font-medium text-sidebar-foreground/90">
              {query.trim() ? t('chat.sidebar.noMatch') : t('chat.sidebar.empty')}
            </p>
            <p className="mt-1 text-[11px] leading-5 text-sidebar-muted">
              {query.trim() ? t('chat.sidebar.noMatchHint') : t('chat.sidebar.emptyHint')}
            </p>
          </div>
        ) : (
          <nav aria-label={t('chat.sidebar.navAria')} className="flex flex-col gap-2">
            {groups.map((group) => (
              <div key={group.key} className="flex flex-col gap-0.5">
                <h3 className="px-2 pb-0.5 pt-1 text-[11px] font-medium text-sidebar-muted">
                  {t(`chat.sidebar.groups.${group.key}`)}
                </h3>
                {group.items.map((session) => (
                  <SessionItem
                    key={session.id}
                    session={session}
                    active={session.id === activeId}
                    busy={busyId === session.id}
                    onRename={handleRename}
                    onArchive={handleArchive}
                    onExport={handleExport}
                    onShare={handleShare}
                    onDelete={handleDelete}
                  />
                ))}
              </div>
            ))}
          </nav>
        )}
      </div>

      <SidebarUserMenu />
    </div>
  )
}

export default SessionSidebar
