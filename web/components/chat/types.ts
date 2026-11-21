/**
 * 聊天页的视图模型。
 *
 * 这一层只做「线上数据结构 → 渲染友好结构」的收敛：
 * - 历史消息（`ChatMessage`）与流式事件（`message.delta` / `tool.call` / …）
 *   都折叠成同一种 `MessageView`，按 `id` 去重、按到达顺序排列；
 * - 工具调用按 `tool_call_id` upsert，避免 `tool.call` / `tool.result` /
 *   历史快照三路重复；
 * - 审批队列与加载错误各有一个小型状态对象。
 *
 * 不允许把 `ChatMessage` 直接传进组件：组件只认这里的类型。
 */

import type { ApprovalRequiredData } from '@/lib/types'

// ── 工具调用 ────────────────────────────────────────────────────────────

export type ToolStatus = 'pending' | 'running' | 'success' | 'error' | 'denied'

export interface ToolCallView {
  tool_call_id: string
  name: string
  args?: unknown
  status: ToolStatus
  result?: unknown
  error?: string | null
}

/** 服务端/历史里的状态字符串归一化。 */
export function toolStatusFrom(value: string | null | undefined): ToolStatus {
  const status = (value ?? '').toLowerCase()
  if (status === 'ok' || status === 'success' || status === 'completed' || status === 'done') {
    return 'success'
  }
  if (status === 'error' || status === 'failed' || status === 'failure') return 'error'
  if (status === 'denied' || status === 'rejected' || status === 'cancelled') return 'denied'
  if (status === 'running' || status === 'in_progress' || status === 'started') return 'running'
  return 'pending'
}

/** 状态归一化之后，展示文案由 i18n 的 `chat.tool.status.*` 提供。 */
// ── 消息 ────────────────────────────────────────────────────────────────

export interface MessageView {
  /** 服务端 `message_id`；乐观消息在服务端确认前是本地临时 id。 */
  id: string
  role: string
  kind: string
  text: string
  /** 只有 assistant 会有：思考过程（`reasoning.delta` 累积）。 */
  reasoning: string
  tool_calls: ToolCallView[]
  created_at: string
  /** 仍在接收 delta。 */
  streaming: boolean
  /** 乐观插入、等待服务端确认的用户消息。 */
  pending: boolean
  /** 服务端确认失败（当前实现会直接移除，保留字段供扩展）。 */
  failed: boolean
}

export function isUserMessage(message: Pick<MessageView, 'role'>): boolean {
  return message.role === 'user'
}

/** 历史消息里 `content` 是联合类型，统一抽成纯文本。 */export function textFromContent(content: unknown): string {
  if (typeof content === 'string') return content
  if (content === null || content === undefined) return ''
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part === 'string') return part
        if (part && typeof part === 'object' && typeof (part as { text?: unknown }).text === 'string') {
          return (part as { text: string }).text
        }
        return ''
      })
      .join('')
  }
  if (typeof content === 'object') {
    const text = (content as { text?: unknown }).text
    if (typeof text === 'string') return text
  }
  return ''
}

// ── 审批 ────────────────────────────────────────────────────────────────

export interface ApprovalView extends ApprovalRequiredData {
  /** 入队时间（毫秒），用于排序与显示「刚刚」。 */
  receivedAt: number
}

// ── 加载错误 ────────────────────────────────────────────────────────────

export type ChatErrorKind =
  | 'unauthorized'
  | 'forbidden'
  | 'not_found'
  | 'rate_limited'
  | 'network'
  | 'unknown'

export interface ChatErrorState {
  kind: ChatErrorKind
  status: number
  code: string
  message: string
}

// ── 运行状态 ────────────────────────────────────────────────────────────

/** `queued/running/waiting_*` 都算「有进行中的 run」。 */
export function isActiveRunStatus(status: string | null | undefined): boolean {
  return (
    status === 'queued' ||
    status === 'running' ||
    status === 'waiting_approval' ||
    status === 'waiting_user'
  )
}
