'use client'

/**
 * 消息列表：滚动容器 + 自动跟随 + 「回到底部」。
 *
 * 滚动策略：
 * - 只在用户停留在底部附近（阈值 80px）时跟随新内容，一旦上滚就停止自动滚动；
 * - 消息数超过 60 时，给「非流式」消息加 `content-visibility: auto` 让浏览器跳过屏外布局
 *   （正在流式的气泡不加，否则高度估算会导致滚动定位跳动）；
 * - 列表用 `role="log"` 但 `aria-live="off"`：逐 token 的 delta 如果开播报会把读屏刷屏，
 *   完成状态改由一个独立的 polite 区域播报一次。
 */

import { useCallback, useEffect, useRef, useState, type CSSProperties } from 'react'
import { Button, Skeleton } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { ArrowDownIcon, ChironMark } from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'
import { MessageBubble } from './MessageBubble'
import { MessageAnchorRail } from './MessageAnchorRail'
import type { MessageView } from './types'

const BOTTOM_THRESHOLD = 80
const HEAVY_LIST_THRESHOLD = 60

const HEAVY_ITEM_STYLE: CSSProperties = {
  contentVisibility: 'auto',
  containIntrinsicSize: 'auto 180px',
}

function LoadingSkeleton() {
  const { t } = useI18n()
  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-6 py-4">
      <span className="sr-only">{t('chat.list.loading')}</span>
      <div className="flex justify-end">
        <Skeleton className="h-10 w-2/5 rounded-2xl" />
      </div>
      <div className="flex flex-col gap-2">
        <Skeleton className="h-3 w-24" />
        <div className="flex flex-col gap-2 rounded-2xl border border-border p-4">
          <Skeleton className="h-3 w-11/12" />
          <Skeleton className="h-3 w-9/12" />
          <Skeleton className="h-3 w-7/12" />
        </div>
      </div>
      <div className="flex justify-end">
        <Skeleton className="h-10 w-1/3 rounded-2xl" />
      </div>
    </div>
  )
}

function EmptyState() {
  const { t } = useI18n()
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-20 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-full border border-border bg-surface text-accent">
        <ChironMark size={22} />
      </span>
      <p className="text-sm font-medium text-foreground">{t('chat.list.emptyTitle')}</p>
      <p className="max-w-sm text-xs text-muted">{t('chat.list.emptyDescription')}</p>
    </div>
  )
}

export interface MessageListProps {
  messages: MessageView[]
  loading: boolean
  className?: string
}

export function MessageList({ messages, loading, className }: MessageListProps) {
  const { t } = useI18n()
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const stickyRef = useRef(true)
  const [showJump, setShowJump] = useState(false)
  const heavy = messages.length > HEAVY_LIST_THRESHOLD

  // 流式结束时的读屏播报（每帧 delta 不播报，只在完成后播一次）
  const [announcement, setAnnouncement] = useState('')
  const last = messages[messages.length - 1]
  const prevLastRef = useRef<{ id?: string; streaming: boolean }>({ streaming: false })
  useEffect(() => {
    const prev = prevLastRef.current
    const streaming = Boolean(last?.streaming)
    // 只有「同一条消息从 streaming 变完成」才播报，切换会话/重载不误报
    if (prev.streaming && !streaming && last && last.id === prev.id && last.role === 'assistant') {
      const plain = last.text.replace(/\s+/g, ' ').trim()
      setAnnouncement(
        plain
          ? t('chat.list.completedWithText', { text: plain.slice(0, 120) })
          : t('chat.list.completed'),
      )
    }
    prevLastRef.current = { id: last?.id, streaming }
  }, [last, t])

  const syncSticky = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight
    const near = distance <= BOTTOM_THRESHOLD
    stickyRef.current = near
    setShowJump(!near)
  }, [])

  const scrollToBottom = useCallback((behavior: ScrollBehavior = 'auto') => {
    const el = scrollRef.current
    if (!el) return
    el.scrollTo({ top: el.scrollHeight, behavior })
    stickyRef.current = true
    setShowJump(false)
  }, [])

  // 跟随：每次消息更新（delta flush / 新增消息）都把视口贴到底部，前提是用户没有上滚
  useEffect(() => {
    if (!stickyRef.current) return
    const el = scrollRef.current
    if (!el) return
    el.scrollTop = el.scrollHeight
  }, [messages])

  // 首屏 / 重载完成后回到底部
  useEffect(() => {
    if (loading) {
      stickyRef.current = true
      return
    }
    const frame = window.requestAnimationFrame(() => scrollToBottom())
    return () => window.cancelAnimationFrame(frame)
  }, [loading, scrollToBottom])

  return (
    <div className={cn('relative min-h-0 flex-1', className)}>
      {/* 完成后播报一次；streaming 期间保持安静，避免逐 token 刷屏 */}
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>
      <div
        ref={scrollRef}
        onScroll={syncSticky}
        role="log"
        aria-live="off"
        aria-busy={loading}
        className="h-full overflow-y-auto overscroll-contain"
      >
        <div className="chat-shell chat-messages">
          {loading ? (
            <LoadingSkeleton />
          ) : messages.length === 0 ? (
            <EmptyState />
          ) : (
            messages.map((message) => (
              <div
                key={message.id}
                data-message-id={message.id}
                style={heavy && !message.streaming ? HEAVY_ITEM_STYLE : undefined}
              >
                <MessageBubble message={message} />
              </div>
            ))
          )}
        </div>
      </div>

      <MessageAnchorRail messages={messages} scrollRef={scrollRef} />

      {showJump ? (
        <div className="pointer-events-none absolute inset-x-0 bottom-4 flex justify-center">
          <Button
            size="sm"
            variant="secondary"
            isIconOnly
            aria-label={t('chat.list.jumpToBottom')}
            onPress={() => scrollToBottom('smooth')}
            className="pointer-events-auto h-10 w-10 shadow-md"
          >
            <ArrowDownIcon size={15} />
          </Button>
        </div>
      ) : null}
    </div>
  )
}
