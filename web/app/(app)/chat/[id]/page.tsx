'use client'

/**
 * 聊天页：Header（会话标题 / 模型）+ 消息流 + Composer + 审批弹窗。
 *
 * 页面只做编排，所有数据与状态机在 `useChatStream`；侧栏由 `(app)` 布局提供。
 * 加载骨架、404 / 429 / 网络错误的重试入口也放在这里。
 */

import { useEffect, useRef } from 'react'
import { useParams } from 'next/navigation'
import Link from 'next/link'
import { Button, Skeleton } from '@heroui/react'
import { ApprovalDialog } from '@/components/chat/ApprovalDialog'
import { ChatHeader } from '@/components/chat/ChatHeader'
import { Composer } from '@/components/chat/Composer'
import { MessageList } from '@/components/chat/MessageList'
import { useChatStream } from '@/components/chat/useChatStream'
import { useVisualViewport } from '@/components/chat/useVisualViewport'
import {
  DisplaySettingsProvider,
  displayVars,
  useDisplaySettings,
} from '@/components/chat/DisplaySettings'
import type { ChatErrorState } from '@/components/chat/types'
import { useI18n } from '@/lib/i18n'
import { playChatSound } from '@/lib/sounds'
import { AlertIcon, ChevronLeftIcon, RefreshIcon } from '@/components/ui-bridge/icons'

function PageSkeleton() {
  const { t } = useI18n()
  return (
    <>
      <header className="flex h-14 shrink-0 items-center gap-3 border-b border-border bg-background/85 px-4 backdrop-blur">
        <Skeleton className="h-3.5 w-44" />
        <div className="flex-1" />
        <Skeleton className="h-7 w-24 rounded-md" />
      </header>
      <div className="min-h-0 flex-1 overflow-hidden">
        <div
          role="status"
          aria-live="polite"
          aria-busy="true"
          className="chat-shell flex flex-col gap-6 py-8"
        >
          <span className="sr-only">{t('chat.page.loading')}</span>
          <div className="flex justify-end">
            <Skeleton className="h-10 w-2/5 rounded-2xl" />
          </div>
          <div className="flex flex-col gap-2">
            <Skeleton className="h-3 w-24" />
            <div className="flex flex-col gap-2 rounded-2xl border border-border p-4">
              <Skeleton className="h-3 w-11/12" />
              <Skeleton className="h-3 w-9/12" />
              <Skeleton className="h-3 w-10/12" />
              <Skeleton className="h-3 w-6/12" />
            </div>
          </div>
          <div className="flex justify-end">
            <Skeleton className="h-10 w-1/3 rounded-2xl" />
          </div>
        </div>
      </div>
      <div className="shrink-0 border-t border-border bg-background px-4 py-3">
        <div className="chat-shell">
          <Skeleton className="h-16 w-full rounded-lg" />
        </div>
      </div>
    </>
  )
}

function errorCopy(
  error: ChatErrorState,
  t: (key: string, vars?: Record<string, string | number>) => string,
): { title: string; description: string } {
  switch (error.kind) {
    case 'not_found':
      return {
        title: t('chat.page.errorNotFoundTitle'),
        description: t('chat.page.errorNotFoundDescription'),
      }
    case 'rate_limited':
      return {
        title: t('chat.page.errorRateLimitedTitle'),
        description: error.message || t('chat.page.errorRateLimitedDescription'),
      }
    case 'network':
      return {
        title: t('chat.page.errorNetworkTitle'),
        description: error.message || t('chat.page.errorNetworkDescription'),
      }
    case 'unauthorized':
      return {
        title: t('chat.page.errorUnauthorizedTitle'),
        description: t('chat.page.errorUnauthorizedDescription'),
      }
    case 'forbidden':
      return {
        title: t('chat.page.errorForbiddenTitle'),
        description: t('chat.page.errorForbiddenDescription'),
      }
    default:
      return {
        title: t('chat.page.errorUnknownTitle'),
        description: error.message || t('chat.page.errorUnknownDescription'),
      }
  }
}

function ErrorState({
  error,
  onRetry,
}: {
  error: ChatErrorState
  onRetry: () => void
}) {
  const { t } = useI18n()
  const copy = errorCopy(error, t)
  const retryable = error.kind !== 'not_found' && error.kind !== 'unauthorized' && error.kind !== 'forbidden'

  return (
    <div className="flex min-h-0 flex-1 items-center justify-center px-4">
      <div
        role="alert"
        className="flex w-full max-w-md flex-col items-center gap-3 rounded-xl border border-border bg-surface px-6 py-8 text-center shadow-xs"
      >
        <span className="flex h-11 w-11 items-center justify-center rounded-full border border-border bg-surface-secondary text-danger">
          <AlertIcon size={20} />
        </span>
        <h1 className="text-base font-semibold text-foreground">{copy.title}</h1>
        <p className="text-sm text-muted">{copy.description}</p>
        {error.code ? (
          <p className="font-mono text-[11px] text-subtle">
            {error.status ? `HTTP ${error.status} · ` : ''}
            {error.code}
          </p>
        ) : null}
        <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
          {retryable ? (
            <Button variant="secondary" onPress={onRetry}>
              <RefreshIcon size={14} />
              {t('common.retry')}
            </Button>
          ) : null}
          <Link
            href="/sessions"
            className="inline-flex min-h-10 items-center gap-1.5 rounded-md border border-border px-3 text-sm text-foreground transition-colors hover:bg-surface-hover md:min-h-8"
          >
            <ChevronLeftIcon size={14} />
            {t('chat.page.backToSessions')}
          </Link>
        </div>
      </div>
    </div>
  )
}

export default function ChatPage() {
  return (
    <DisplaySettingsProvider>
      <ChatPageInner />
    </DisplaySettingsProvider>
  )
}

function ChatPageInner() {
  const { t } = useI18n()
  const { settings } = useDisplaySettings()
  const viewport = useVisualViewport()
  const params = useParams<{ id: string }>()
  const sessionId = typeof params?.id === 'string' ? params.id : ''
  const prevLastRef = useRef<{ id?: string; streaming: boolean }>({ streaming: false })

  const {
    session,
    messages,
    live,
    running,
    approvals,
    loading,
    error,
    decidingId,
    send,
    steer,
    stop,
    decide,
    reload,
    rename,
    setModel,
  } = useChatStream(sessionId)

  const archived = session ? session.status !== 'active' : false
  const activeApproval = approvals[0] ?? null

  // 命令面板切模型：交给既有 setModel 统一落库 + toast。
  useEffect(() => {
    const handler = (event: Event) => {
      const detail = (event as CustomEvent<{ model?: string }>).detail
      if (detail?.model) void setModel(detail.model)
    }
    window.addEventListener('chiron:change-model', handler)
    return () => window.removeEventListener('chiron:change-model', handler)
  }, [setModel])

  // 交互音效：只在「助手这条消息从流式变完成」时响一次，切换会话/重载不误报。
  useEffect(() => {
    const last = messages[messages.length - 1]
    const previous = prevLastRef.current
    if (settings.sound && previous.streaming && !last?.streaming && last?.role === 'assistant') {
      playChatSound('receive')
    }
    prevLastRef.current = { id: last?.id, streaming: Boolean(last?.streaming) }
  }, [messages, settings.sound])

  async function handleSend(text: string) {
    if (settings.sound) playChatSound('send')
    await send(text)
  }

  return (
    <div
      className="chat-display flex h-dvh min-h-0 flex-col bg-background"
      data-density={settings.density}
      data-keyboard={viewport.keyboardOpen ? 'true' : undefined}
      style={
        viewport.height
          ? { ...displayVars(settings), height: `${viewport.height}px` }
          : displayVars(settings)
      }
    >
      {/* 读屏播报：流式状态变化（视觉上已在气泡里体现） */}
      <p className="sr-only" aria-live="polite">
        {live
          ? t('chat.page.liveReplying')
          : running
            ? t('chat.page.agentRunning')
            : ''}
      </p>

      {loading && !session ? (
        <PageSkeleton />
      ) : error && !session ? (
        <>
          <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border bg-background/85 px-4 backdrop-blur">
            <Link
              href="/sessions"
              aria-label={t('chat.page.backToSessions')}
              className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted transition-colors hover:bg-surface-hover hover:text-foreground md:hidden"
            >
              <ChevronLeftIcon size={16} />
            </Link>
            <span className="text-sm font-semibold text-foreground">
              {t('chat.page.sessionLabel')}
            </span>
          </header>
          <ErrorState error={error} onRetry={reload} />
        </>
      ) : (
        <>
          <ChatHeader
            session={session}
            running={running}
            waitingApproval={approvals.length > 0}
            onRename={rename}
            onModelChange={setModel}
          />

          {error ? (
            <div
              role="alert"
              className="flex items-center gap-2 border-b border-border bg-danger/5 px-4 py-2 text-xs text-danger"
            >
              <AlertIcon size={13} className="shrink-0" />
              <span className="min-w-0 flex-1 truncate">{error.message}</span>
              <Button size="sm" variant="ghost" onPress={reload}>
                {t('common.retry')}
              </Button>
            </div>
          ) : null}

          <MessageList messages={messages} loading={loading && messages.length === 0} />

          <Composer
            running={running}
            disabled={archived}
            placeholder={archived ? t('chat.page.archivedPlaceholder') : undefined}
            onSend={handleSend}
            onSteer={steer}
            onStop={stop}
          />
        </>
      )}

      <ApprovalDialog
        approval={activeApproval}
        deciding={decidingId !== null}
        pendingCount={Math.max(0, approvals.length - 1)}
        onDecide={decide}
      />
    </div>
  )
}
