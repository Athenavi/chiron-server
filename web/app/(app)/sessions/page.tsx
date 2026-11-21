'use client'

/**
 * 会话入口页。
 *
 * - 拉最近一条活跃会话：存在就 `router.replace('/chat/{id}')`；
 * - 一条都没有时展示 EmptyState + 「新建会话」主按钮；
 * - 加载 / 错误各自有骨架与重试，401 由 `(app)/layout.tsx` 的认证守卫处理。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import { Button, EmptyState, Skeleton, toast } from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { MessageIcon, PlusIcon, RefreshIcon } from '@/components/ui-bridge/icons'

type PageState = 'loading' | 'redirecting' | 'empty' | 'error'

function ListSkeleton() {
  const { t } = useI18n()
  return (
    <div
      role="status"
      aria-live="polite"
      aria-busy="true"
      className="mt-6 rounded-lg border border-border bg-surface p-4 shadow-xs"
    >
      <span className="sr-only">{t('chat.sessions.opening')}</span>
      <ul className="flex flex-col divide-y divide-border">
        {Array.from({ length: 5 }, (_, index) => (
          <li key={index} className="flex items-center gap-3 py-3.5 first:pt-1 last:pb-1">
            <Skeleton className="size-8 shrink-0 rounded-md" />
            <div className="min-w-0 flex-1">
              <Skeleton className="h-3" style={{ width: `${42 + index * 9}%` }} />
              <Skeleton className="mt-2 h-2.5 w-28" />
            </div>
            <Skeleton className="h-2.5 w-12 shrink-0" />
          </li>
        ))}
      </ul>
    </div>
  )
}

export default function SessionsPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [state, setState] = useState<PageState>('loading')
  const [error, setError] = useState<string | null>(null)
  const [creating, setCreating] = useState(false)
  const redirectedRef = useRef(false)

  const load = useCallback(async () => {
    setState('loading')
    setError(null)
    try {
      const { sessions } = await api.listSessions({ status: 'active', limit: 1 })
      const latest = sessions[0]
      if (latest) {
        if (redirectedRef.current) return
        redirectedRef.current = true
        setState('redirecting')
        router.replace(`/chat/${latest.id}`)
        return
      }
      setState('empty')
    } catch (cause) {
      setError(ApiError.from(cause, t('chat.sessions.loadFailed')).message)
      setState('error')
    }
  }, [router, t])

  useEffect(() => {
    void load()
  }, [load])

  async function handleCreate() {
    if (creating) return
    setCreating(true)
    try {
      const { session } = await api.createSession({})
      router.replace(`/chat/${session.id}`)
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sessions.createFailed')).message)
    } finally {
      setCreating(false)
    }
  }

  return (
    <div className="content-shell py-6 md:py-8">
      <header className="flex flex-wrap items-end justify-between gap-3">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight text-foreground">
            {t('chat.sessions.heading')}
          </h1>
          <p className="mt-1 text-sm text-muted">{t('chat.sessions.subtitle')}</p>
        </div>
        <Button
          variant="primary"
          size="sm"
          isPending={creating}
          onPress={() => void handleCreate()}
          className="h-10 gap-1.5 sm:h-9"
        >
          {creating ? null : <PlusIcon size={15} />}
          {t('chat.sessions.newSession')}
        </Button>
      </header>

      {state === 'loading' || state === 'redirecting' ? <ListSkeleton /> : null}

      {state === 'error' ? (
        <div className="mt-6 flex flex-col items-start gap-3 rounded-lg border border-border bg-surface p-5 shadow-xs">
          <p className="text-sm text-foreground">{t('chat.sessions.listErrorTitle')}</p>
          <p className="text-xs text-muted">{error}</p>
          <Button
            variant="secondary"
            size="sm"
            className="h-10 gap-1.5 sm:h-9"
            onPress={() => void load()}
          >
            <RefreshIcon size={14} />
            {t('common.retry')}
          </Button>
        </div>
      ) : null}

      {state === 'empty' ? (
        <EmptyState className="mt-6 flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
          <span
            aria-hidden="true"
            className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
          >
            <MessageIcon size={20} />
          </span>
          <h2 className="text-sm font-semibold text-foreground">{t('chat.sessions.empty')}</h2>
          <p className="max-w-sm text-xs leading-5 text-muted">{t('chat.sessions.emptyHint')}</p>
          <Button
            variant="primary"
            isPending={creating}
            onPress={() => void handleCreate()}
            className="mt-1 h-10 gap-1.5 px-4"
          >
            {creating ? null : <PlusIcon size={15} />}
            {t('chat.sessions.newSession')}
          </Button>
        </EmptyState>
      ) : null}
    </div>
  )
}
