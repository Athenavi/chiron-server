/**
 * 会话导出与分享。
 *
 * 导出在浏览器侧完成：按游标拉全量消息，生成 Markdown 并触发下载，不需要新后端接口。
 * 分享优先走 Web Share API，不支持或失败时回落复制当前会话链接。
 */

import { api } from './api'
import type { ChatMessage, ChatSession, MessageContent, ToolCall } from './types'
import { textFromContent } from '@/components/chat/types'

const PAGE_LIMIT = 100
const MAX_PAGES = 200

function toolCallsOf(content: MessageContent): ToolCall[] {
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    const raw = (content as { tool_calls?: ToolCall[] | null }).tool_calls
    if (Array.isArray(raw)) return raw
  }
  return []
}

async function loadAllMessages(sessionId: string): Promise<ChatMessage[]> {
  const seen = new Set<string>()
  const out: ChatMessage[] = []
  let cursor: string | null = null
  for (let page = 0; page < MAX_PAGES; page += 1) {
    const result = await api.listMessages(sessionId, {
      limit: PAGE_LIMIT,
      cursor: cursor ?? undefined,
    })
    for (const message of result.messages) {
      if (seen.has(message.id)) continue
      seen.add(message.id)
      out.push(message)
    }
    if (!result.next_cursor) break
    cursor = result.next_cursor
  }
  return out
}

function pad(value: number): string {
  return String(value).padStart(2, '0')
}

function stamp(value: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`
}

function roleLabel(role: string): string {
  if (role === 'user') return 'User'
  if (role === 'assistant') return 'Assistant'
  if (role === 'system') return 'System'
  if (role === 'tool') return 'Tool'
  return role
}

export function buildSessionMarkdown(session: ChatSession, messages: ChatMessage[]): string {
  const title = session.title?.trim() || session.id
  const lines: string[] = [
    `# ${title}`,
    '',
    `- Session: ${session.id}`,
    `- Model: ${session.model || 'default'}`,
    `- Created: ${stamp(session.created_at)}`,
    `- Exported: ${stamp(new Date().toISOString())}`,
    `- Messages: ${messages.length}`,
    '',
  ]

  for (const message of messages) {
    lines.push('---', '', `### ${roleLabel(message.role)} · ${stamp(message.created_at)}`, '')
    const text = textFromContent(message.content).trim()
    if (text) lines.push(text, '')
    for (const call of toolCallsOf(message.content)) {
      const name = call.name || 'tool'
      const status = call.status || call.error || 'done'
      lines.push(`> Tool: \`${name}\` (${status})`, '')
    }
  }

  return `${lines.join('\n').trimEnd()}\n`
}

function safeFilename(title: string, id: string): string {
  const base = (title || id)
    .replace(/[\\/:*?"<>|]+/g, '-')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 80)
  return `${base || 'chiron-session'}.md`
}

/** 拉全量消息并下载 Markdown；返回导出的消息条数。 */
export async function exportSession(session: ChatSession): Promise<number> {
  const messages = await loadAllMessages(session.id)
  const markdown = buildSessionMarkdown(session, messages)
  const blob = new Blob([markdown], { type: 'text/markdown;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  try {
    const link = document.createElement('a')
    link.href = url
    link.download = safeFilename(session.title, session.id)
    document.body.appendChild(link)
    link.click()
    link.remove()
  } finally {
    URL.revokeObjectURL(url)
  }
  return messages.length
}

export type ShareResult = 'shared' | 'copied' | 'cancelled'

/** 分享当前会话链接；Web Share 不可用时复制链接。 */
export async function shareSession(session: ChatSession): Promise<ShareResult> {
  const url = new URL(`/chat/${encodeURIComponent(session.id)}`, window.location.origin).toString()
  const title = session.title?.trim() || session.id

  if (typeof navigator !== 'undefined' && typeof navigator.share === 'function') {
    try {
      await navigator.share({ title, text: title, url })
      return 'shared'
    } catch (error) {
      if (error instanceof DOMException && error.name === 'AbortError') return 'cancelled'
      // 其他错误（权限、无可用目标）回落到复制
    }
  }

  await navigator.clipboard.writeText(url)
  return 'copied'
}
