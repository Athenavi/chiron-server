'use client'

/**
 * 归档会话页。
 *
 * - 首屏拉 `status=archived`（每页 50），按 `updated_at` 倒序渲染卡片；
 * - 恢复：`updateSession(id, { archived: false })`，成功后 toast 并从列表移除（不自动跳转）；
 * - 删除：先确认弹窗，成功（或失败）都在弹窗内给出反馈，成功后移除；
 * - 分页：`next_cursor` 存在时「加载更多」，追加时按 id 去重；
 * - 加载骨架 / 空态 / 错误 Alert + 重试齐备，写操作失败统一 toast 后端 message；
 * - 文案全部走 `archived.*` i18n 命名空间。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import Link from 'next/link'
import { useRouter } from 'next/navigation'
import { Alert, Button, EmptyState, toast } from '@heroui/react'
import { PageHeader } from '@/components/shell/PageHeader'
import { ArchivedListSkeleton } from '@/components/archived/ArchivedListSkeleton'
import { ArchivedSessionItem } from '@/components/archived/ArchivedSessionItem'
import { DeleteSessionDialog } from '@/components/archived/DeleteSessionDialog'
import {
  AlertIcon,
  ArchiveIcon,
  ChevronLeftIcon,
  MessageIcon,
  RefreshIcon,
} from '@/components/ui-bridge/icons'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type ChatSession } from '@/lib/types'

/** 与后端默认分页上限一致。 */
const PAGE_SIZE = 50

/** 按 `updated_at` 倒序排好并去掉重复 id（刷新与加载更多可能返回同一批数据）。 */
function mergeSessions(current: ChatSession[], incoming: ChatSession[]): ChatSession[] {
  const seen = new Set(current.map((item) => item.id))
  const merged = [...current]
  for (const session of incoming) {
    if (seen.has(session.id)) continue
    seen.add(session.id)
    merged.push(session)
  }
  return merged.sort((a, b) => b.updated_at.localeCompare(a.updated_at))
}

export default function ArchivedSessionsPage() {
  const router = useRouter()
  const { t, locale } = useI18n()
  const intlTag = locale === 'en' ? 'en-US' : 'zh-CN'

  const [sessions, setSessions] = useState<ChatSession[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  /** 保留原始错误对象：错误文案在渲染时按当前 locale 生成。 */
  const [error, setError] = useState<unknown>(null)

  const [restoringId, setRestoringId] = useState<string | null>(null)

  const [deleteTarget, setDeleteTarget] = useState<ChatSession | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  /** 刷新 / 加载更多各自的请求序号：慢响应回来时丢弃，避免互相覆盖。 */
  const loadSeq = useRef(0)
  const moreSeq = useRef(0)

  /** 会话展示名：标题为空时统一回落到「未命名会话」。 */
  const sessionLabel = useCallback(
    (session: ChatSession | null) => session?.title?.trim() || t('archived.untitled'),
    [t],
  )

  const load = useCallback(async () => {
    const seq = (loadSeq.current += 1)
    setLoading(true)
    setError(null)
    try {
      const result = await api.listSessions({ status: 'archived', limit: PAGE_SIZE })
      if (seq !== loadSeq.current) return
      setSessions(mergeSessions([], result.sessions))
      setNextCursor(result.next_cursor)
    } catch (cause) {
      if (seq !== loadSeq.current) return
      setError(cause)
    } finally {
      if (seq === loadSeq.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  async function loadMore() {
    const cursor = nextCursor
    if (!cursor || loadingMore || loading) return
    const seq = (moreSeq.current += 1)
    const loadMark = loadSeq.current
    setLoadingMore(true)
    try {
      const result = await api.listSessions({ status: 'archived', cursor, limit: PAGE_SIZE })
      // 期间发生过刷新：丢弃这页，避免把旧游标的数据混进新列表
      if (seq !== moreSeq.current || loadMark !== loadSeq.current) return
      setSessions((previous) => mergeSessions(previous, result.sessions))
      setNextCursor(result.next_cursor)
    } catch (cause) {
      if (seq !== moreSeq.current || loadMark !== loadSeq.current) return
      toast.danger(ApiError.from(cause, t('archived.error.moreFallback')).message)
    } finally {
      if (seq === moreSeq.current) setLoadingMore(false)
    }
  }

  async function handleRestore(session: ChatSession) {
    if (restoringId || deleting) return
    setRestoringId(session.id)
    try {
      await api.updateSession(session.id, { archived: false })
      setSessions((previous) => previous.filter((item) => item.id !== session.id))
      toast.success(t('archived.restored', { title: sessionLabel(session) }))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('archived.restoreFailed')).message)
    } finally {
      setRestoringId(null)
    }
  }

  function openDelete(session: ChatSession) {
    setDeleteError(null)
    setDeleteTarget(session)
  }

  function closeDelete() {
    if (deleting) return
    setDeleteTarget(null)
    setDeleteError(null)
  }

  async function runDelete() {
    const target = deleteTarget
    if (!target || deleting) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await api.deleteSession(target.id)
      setSessions((previous) => previous.filter((item) => item.id !== target.id))
      setDeleteTarget(null)
      toast.success(t('archived.deleted', { title: sessionLabel(target) }))
    } catch (cause) {
      const message = ApiError.from(cause, t('archived.deleteFailed')).message
      setDeleteError(message)
      toast.danger(message)
    } finally {
      setDeleting(false)
    }
  }

  const errorMessage = error ? ApiError.from(error, t('archived.error.fallback')).message : null
  const isEmpty = !loading && !errorMessage && sessions.length === 0

  return (
    <div>
      <PageHeader
        title={t('archived.title')}
        description={t('archived.description')}
        actions={
          <>
            <Button
              variant="secondary"
              size="sm"
              className="h-9 gap-1.5"
              isPending={loading}
              onPress={() => void load()}
            >
              {loading ? null : <RefreshIcon size={14} />}
              {t('common.refresh')}
            </Button>
            <Link
              href="/sessions"
              className="inline-flex h-9 items-center gap-1 rounded-full px-2.5 text-sm font-medium text-muted transition-colors hover:bg-surface-hover hover:text-foreground"
            >
              <ChevronLeftIcon size={14} aria-hidden="true" />
              {t('archived.backToChat')}
            </Link>
          </>
        }
      />

      <div className="content-shell max-w-5xl py-6 md:py-8">
        {loading ? <ArchivedListSkeleton /> : null}

        {!loading && errorMessage ? (
          <div className="flex flex-col items-start gap-3">
            <Alert status="danger" className="w-full">
              <Alert.Indicator>
                <AlertIcon aria-hidden="true" className="size-4" />
              </Alert.Indicator>
              <Alert.Content>
                <Alert.Title>{t('archived.error.title')}</Alert.Title>
                <Alert.Description>{errorMessage}</Alert.Description>
              </Alert.Content>
            </Alert>
            <Button
              variant="secondary"
              size="sm"
              className="h-9 gap-1.5"
              onPress={() => void load()}
            >
              <RefreshIcon size={14} />
              {t('common.retry')}
            </Button>
          </div>
        ) : null}

        {isEmpty ? (
          <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <ArchiveIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('archived.empty.title')}</h2>
            <p className="max-w-sm text-xs leading-5 text-muted">{t('archived.empty.body')}</p>
            <Button
              variant="primary"
              className="mt-1 h-10 gap-1.5 px-4"
              onPress={() => router.push('/sessions')}
            >
              <MessageIcon size={15} />
              {t('archived.empty.go')}
            </Button>
          </EmptyState>
        ) : null}

        {!loading && !errorMessage && sessions.length > 0 ? (
          <>
            <div className="flex flex-wrap items-center justify-between gap-2">
              <p className="text-xs text-muted tabular" aria-live="polite">
                {t('archived.count', {
                  n: new Intl.NumberFormat(intlTag).format(sessions.length),
                })}
              </p>
            </div>

            <ul aria-label={t('archived.listLabel')} className="mt-3 flex flex-col gap-3">
              {sessions.map((session) => (
                <li key={session.id}>
                  <ArchivedSessionItem
                    session={session}
                    isRestoring={restoringId === session.id}
                    isBusy={deleting}
                    onRestore={(target) => void handleRestore(target)}
                    onDelete={openDelete}
                  />
                </li>
              ))}
            </ul>

            {nextCursor ? (
              <div className="mt-4 flex justify-center">
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-9 px-4"
                  isPending={loadingMore}
                  isDisabled={restoringId !== null || deleting}
                  onPress={() => void loadMore()}
                >
                  {loadingMore ? null : <RefreshIcon size={13} />}
                  {t('archived.loadMore')}
                </Button>
              </div>
            ) : null}
          </>
        ) : null}
      </div>

      <DeleteSessionDialog
        open={deleteTarget !== null}
        title={sessionLabel(deleteTarget)}
        isPending={deleting}
        error={deleteError}
        onOpenChange={(open) => {
          if (!open) closeDelete()
        }}
        onConfirm={() => void runDelete()}
      />
    </div>
  )
}
