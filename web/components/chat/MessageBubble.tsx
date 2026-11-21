'use client'

/**
 * 单条消息气泡。
 *
 * - user：靠右，accent 软底（`.bubble-user`），保留换行；
 * - assistant / system / tool：靠左，surface 底 + 细边框（`.bubble-assistant`），
 *   上方是 Chiron 头像 + 时间；正文用自实现 Markdown，工具卡紧跟在正文下方。
 *
 * 组件是 memo 的：流式 flush 只替换「当前那条」消息对象，其余气泡不重渲染。
 */

import { memo } from 'react'
import { Chip } from '@heroui/react'
import { ChironMark } from '@/components/ui-bridge/icons'
import { useI18n, type Locale } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import { Markdown } from './Markdown'
import { ReasoningPanel } from './ReasoningPanel'
import { StreamingCursor } from './StreamingCursor'
import { ToolCallCard } from './ToolCallCard'
import type { MessageView } from './types'

function formatClock(value: string, locale: Locale): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  return date.toLocaleTimeString(locale === 'en' ? 'en-US' : 'zh-CN', {
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

export interface MessageBubbleProps {
  message: MessageView
  className?: string
}

export const MessageBubble = memo(function MessageBubble({ message, className }: MessageBubbleProps) {
  const { t, locale } = useI18n()
  const isUser = message.role === 'user'
  const clock = formatClock(message.created_at, locale)
  const hasTools = message.tool_calls.length > 0
  const sender =
    message.role === 'assistant'
      ? 'Chiron'
      : message.role === 'system'
        ? t('chat.bubble.senderSystem')
        : message.role === 'tool'
          ? t('chat.bubble.senderTool')
          : message.role

  if (isUser) {
    return (
      <div className={cn('flex w-full justify-end', className)}>
        <div className="flex max-w-[85%] flex-col items-end gap-1 md:max-w-[75%]">
          <div className="bubble-user chat-bubble chat-message-text rounded-2xl rounded-br-md text-foreground shadow-xs">
            <p className="whitespace-pre-wrap break-words">{message.text}</p>
          </div>
          <div className="flex items-center gap-2 pr-1 text-[11px] text-subtle">
            {message.kind === 'steer' ? (
              <Chip size="sm" variant="soft">
                {t('chat.bubble.steer')}
              </Chip>
            ) : null}
            {message.pending ? <span>{t('chat.bubble.sending')}</span> : null}
            {clock ? (
              <time dateTime={message.created_at} className="tabular">
                {clock}
              </time>
            ) : null}
          </div>
        </div>
      </div>
    )
  }

  return (
    <div className={cn('flex w-full justify-start', className)}>
      <div className="flex w-full max-w-[95%] flex-col gap-1.5 md:max-w-[85%]">
        <div className="flex items-center gap-2 pl-0.5">
          <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-border bg-surface text-accent">
            <ChironMark size={12} />
          </span>
          <span className="text-xs font-medium text-muted">{sender}</span>
          {clock ? (
            <time dateTime={message.created_at} className="tabular text-[11px] text-subtle">
              {clock}
            </time>
          ) : null}
          {message.streaming ? (
            <span className="text-[11px] text-subtle" aria-hidden="true">
              {t('chat.bubble.generating')}
            </span>
          ) : null}
        </div>

        {message.reasoning ? (
          <ReasoningPanel text={message.reasoning} streaming={message.streaming} />
        ) : null}

        {message.text ? (
          <div className="bubble-assistant chat-bubble rounded-2xl rounded-tl-md border border-border shadow-xs">
            <Markdown text={message.text} />
            {message.streaming ? (
              <div className="mt-1">
                <StreamingCursor />
              </div>
            ) : null}
          </div>
        ) : message.streaming ? (
          <div className="bubble-assistant chat-bubble inline-flex w-fit items-center gap-2 rounded-2xl rounded-tl-md border border-border chat-message-text text-muted shadow-xs">
            <StreamingCursor label={t('chat.bubble.thinking')} />
            <span>{t('chat.bubble.thinking')}</span>
          </div>
        ) : (
          <p className="text-xs italic text-subtle">{t('chat.bubble.empty')}</p>
        )}

        {hasTools ? (
          <div className="flex flex-col gap-2">
            {message.tool_calls.map((call) => (
              <ToolCallCard key={call.tool_call_id} tool={call} />
            ))}
          </div>
        ) : null}
      </div>
    </div>
  )
})
