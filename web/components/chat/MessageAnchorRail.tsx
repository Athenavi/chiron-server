'use client'

/**
 * 消息快速跳转锚点（右侧「钢琴键」）。
 *
 * - 消息超过两条才出现；长会话按固定上限抽样，避免锚点溢出屏幕；
 * - 每根键代表一条消息，点击滚动到消息中部；滚动时高亮当前所在消息；
 * - 宽度/动画延迟由消息文本哈希决定，形成轻微律动；`prefers-reduced-motion` 下停用。
 */

import { useCallback, useEffect, useMemo, useState, type CSSProperties, type RefObject } from 'react'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import type { MessageView } from './types'

const MAX_ANCHORS = 28
const ACTIVE_OFFSET = 96

interface Anchor {
  id: string
  index: number
  preview: string
  width: number
}

function previewOf(message: MessageView): string {
  const text = message.text.replace(/\s+/g, ' ').trim()
  if (text) return text.slice(0, 60)
  if (message.tool_calls.length > 0) return message.tool_calls[0].name
  return message.role
}

function hash(value: string): number {
  let result = 0
  for (let index = 0; index < value.length; index += 1) {
    result = (result * 31 + value.charCodeAt(index)) >>> 0
  }
  return result
}

function buildAnchors(messages: MessageView[]): Anchor[] {
  if (messages.length <= 2) return []
  const step = messages.length > MAX_ANCHORS ? (messages.length - 1) / (MAX_ANCHORS - 1) : 1
  const count = messages.length > MAX_ANCHORS ? MAX_ANCHORS : messages.length
  const anchors: Anchor[] = []
  for (let slot = 0; slot < count; slot += 1) {
    const index = Math.round(slot * step)
    const message = messages[index]
    if (!message) continue
    const seed = hash(`${message.id}:${message.role}`)
    anchors.push({
      id: message.id,
      index,
      preview: previewOf(message),
      width: 9 + (seed % 13),
    })
  }
  return anchors
}

function messageNode(container: HTMLElement, id: string): HTMLElement | null {
  const nodes = container.querySelectorAll<HTMLElement>('[data-message-id]')
  for (const node of nodes) {
    if (node.dataset.messageId === id) return node
  }
  return null
}

export interface MessageAnchorRailProps {
  messages: MessageView[]
  scrollRef: RefObject<HTMLDivElement | null>
}

export function MessageAnchorRail({ messages, scrollRef }: MessageAnchorRailProps) {
  const { t } = useI18n()
  const lastId = messages[messages.length - 1]?.id ?? ''
  const signature = `${messages.length}:${lastId}`
  // 流式 delta 不会改变 anchor 集合；预览文本取最近一次构建时的快照即可。
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const anchors = useMemo(() => buildAnchors(messages), [signature])
  const [activeId, setActiveId] = useState<string | null>(null)

  useEffect(() => {
    const container = scrollRef.current
    if (!container || anchors.length === 0) {
      setActiveId(null)
      return
    }
    let frame = 0

    const update = () => {
      frame = 0
      const line = container.getBoundingClientRect().top + ACTIVE_OFFSET
      let current: string | null = anchors[0]?.id ?? null
      for (const anchor of anchors) {
        const node = messageNode(container, anchor.id)
        if (!node) continue
        if (node.getBoundingClientRect().top <= line) current = anchor.id
        else break
      }
      setActiveId((previous) => (previous === current ? previous : current))
    }

    const onScroll = () => {
      if (frame) return
      frame = window.requestAnimationFrame(update)
    }

    container.addEventListener('scroll', onScroll, { passive: true })
    update()
    return () => {
      container.removeEventListener('scroll', onScroll)
      if (frame) window.cancelAnimationFrame(frame)
    }
  }, [anchors, scrollRef])

  const jump = useCallback(
    (anchor: Anchor) => {
      const container = scrollRef.current
      if (!container) return
      const node = messageNode(container, anchor.id)
      if (!node) return
      setActiveId(anchor.id)
      node.scrollIntoView({ behavior: 'smooth', block: 'center' })
    },
    [scrollRef],
  )

  if (anchors.length === 0) return null

  return (
    <div className="pointer-events-none absolute right-1 top-1/2 z-10 hidden -translate-y-1/2 md:block">
      <div
        role="group"
        aria-label={t('chat.list.anchorGroup')}
        className="pointer-events-auto flex max-h-[60vh] flex-col items-end gap-[3px] overflow-hidden rounded-full border border-border/70 bg-background/75 px-1 py-2 shadow-sm backdrop-blur-sm"
      >
        {anchors.map((anchor, index) => {
          const active = anchor.id === activeId
          return (
            <button
              key={anchor.id}
              type="button"
              aria-label={t('chat.list.anchorAria', {
                n: anchor.index + 1,
                text: anchor.preview,
              })}
              aria-current={active ? 'true' : undefined}
              onClick={() => jump(anchor)}
              className={cn('piano-key', active && 'piano-key-active')}
              style={
                {
                  '--piano-width': `${anchor.width}px`,
                  animationDelay: `${(index % 9) * 0.14}s`,
                } as CSSProperties
              }
            />
          )
        })}
      </div>
    </div>
  )
}
