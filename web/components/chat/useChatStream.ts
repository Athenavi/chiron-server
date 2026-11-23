'use client'

/**
 * `useChatStream`：聊天页的数据与状态机。
 *
 * 职责边界：
 * - 首次加载：并行 `getSession` + `listMessages`，把错误分类成可渲染的 `ChatErrorState`；
 * - 订阅：`subscribeSessionEvents` 的单一入口，所有事件在 `applyEvent` 里折叠到消息视图；
 * - 合并策略：
 *   · 消息按 `message_id` 去重（历史快照 / 回放事件 / 乐观消息三条来源）；
 *   · 工具调用按 `tool_call_id` upsert；
 *   · `message.delta` 先进 buffer，用 `requestAnimationFrame` 合批 flush，
 *     避免每个 token 一次 setState；
 * - 动作：`send` / `steer` / `stop` / `decide` / `reload` / `rename` / `setModel`。
 *
 * 服务端不需要知道视图模型：这里把 `content` 联合类型统一压成纯文本 + 工具卡片。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import { toast } from '@heroui/react'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { subscribeSessionEvents } from '@/lib/sse'
import {
  ApiError,
  type ApprovalDecision,
  type ChatMessage,
  type ChatSession,
  type SessionEvent,
  type ToolCall,
} from '@/lib/types'
import {
  isActiveRunStatus,
  textFromContent,
  toolStatusFrom,
  type ApprovalView,
  type ChatErrorState,
  type MessageView,
  type ToolCallView,
  type ToolStatus,
} from './types'

const HISTORY_LIMIT = 100
const APPROVAL_SWEEP_MS = 1000

// ── 纯函数：视图构造与合并 ──────────────────────────────────────────────

function nowIso(): string {
  return new Date().toISOString()
}

function makeLocalId(prefix: string): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return `${prefix}_${crypto.randomUUID()}`
  }
  return `${prefix}_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`
}

function toolCallIdOf(call: ToolCall, index: number): string {
  return call.tool_call_id || call.id || `call_${index}`
}

function toToolView(call: ToolCall, index: number, fallbackName: string): ToolCallView {
  return {
    tool_call_id: toolCallIdOf(call, index),
    name: call.name || fallbackName,
    args: call.args !== undefined ? call.args : call.arguments,
    status: toolStatusFrom(call.status),
    result: call.result,
    error: call.error ?? null,
  }
}

/** 终态一旦到过就不再回退成 running/pending（tool.result 可能先于 tool.call 到达）。 */
function isTerminalToolStatus(status: ToolStatus): boolean {
  return status === 'success' || status === 'error' || status === 'denied'
}

function mergeToolStatus(current: ToolStatus, incoming: ToolStatus): ToolStatus {
  if (isTerminalToolStatus(current) && !isTerminalToolStatus(incoming)) return current
  if (incoming === 'pending') return current
  return incoming
}

function upsertToolCall(list: ToolCallView[], incoming: ToolCallView): ToolCallView[] {
  const index = list.findIndex((item) => item.tool_call_id === incoming.tool_call_id)
  if (index === -1) return [...list, incoming]
  const current = list[index]
  const merged: ToolCallView = {
    ...current,
    ...incoming,
    name: incoming.name || current.name,
    args: incoming.args !== undefined ? incoming.args : current.args,
    status: mergeToolStatus(current.status, incoming.status),
    result: incoming.result !== undefined ? incoming.result : current.result,
    error: incoming.error ?? current.error,
  }
  return [...list.slice(0, index), merged, ...list.slice(index + 1)]
}

function mergeToolLists(base: ToolCallView[], extra: ToolCallView[]): ToolCallView[] {
  let next = base
  for (const call of extra) next = upsertToolCall(next, call)
  return next
}

function toView(message: ChatMessage, fallbackName: string): MessageView {
  const content = message.content
  let calls: ToolCall[] = []
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    const raw = (content as { tool_calls?: ToolCall[] | null }).tool_calls
    if (Array.isArray(raw)) calls = raw
  }
  return {
    id: message.id,
    role: message.role,
    kind: message.kind,
    text: textFromContent(content),
    reasoning: '',
    tool_calls: calls.map((call, index) => toToolView(call, index, fallbackName)),
    created_at: message.created_at,
    streaming: false,
    pending: false,
    failed: false,
  }
}

function newAssistantView(id: string): MessageView {
  return {
    id,
    role: 'assistant',
    kind: 'chat',
    text: '',
    reasoning: '',
    tool_calls: [],
    created_at: nowIso(),
    streaming: true,
    pending: false,
    failed: false,
  }
}

function sortByCreatedAt(list: MessageView[]): MessageView[] {
  return list.slice().sort((a, b) => {
    const left = Date.parse(a.created_at)
    const right = Date.parse(b.created_at)
    if (Number.isNaN(left) || Number.isNaN(right) || left === right) return 0
    return left - right
  })
}

/** 把占位消息（或空 id）的视图 id 迁到真实 `message_id`，两边都有时合并。 */
function adoptViewId(list: MessageView[], oldId: string, newId: string): MessageView[] {
  const placeholder = oldId ? list.find((item) => item.id === oldId) : undefined
  const target = list.find((item) => item.id === newId)
  if (!placeholder) {
    if (target) return list
    return [...list, newAssistantView(newId)]
  }
  if (!target) {
    return list.map((item) => (item.id === oldId ? { ...item, id: newId } : item))
  }
  // 两边都有：目标已在流式中就以目标为准，否则用占位消息「重新开始」——
  // 这样回放的 reasoning.delta 先到时，不会把历史终稿和新 delta 拼成双份。
  return list
    .filter((item) => item.id !== oldId)
    .map((item) =>
      item.id === newId
        ? {
            ...item,
            text: item.streaming ? item.text || placeholder.text : placeholder.text || item.text,
            reasoning: item.streaming
              ? item.reasoning || placeholder.reasoning
              : placeholder.reasoning || item.reasoning,
            tool_calls: mergeToolLists(item.tool_calls, placeholder.tool_calls),
            streaming: true,
          }
        : item,
    )
}

function promoteLocalView(list: MessageView[], localId: string, serverId: string): MessageView[] {
  const local = list.find((item) => item.id === localId)
  if (!local) return list
  if (list.some((item) => item.id === serverId)) {
    return list.filter((item) => item.id !== localId)
  }
  return list.map((item) => (item.id === localId ? { ...item, id: serverId, pending: false } : item))
}

function classifyError(error: unknown): ChatErrorState {
  const err = ApiError.from(error)
  if (err.status === 401 || err.code === 'unauthorized') {
    return { kind: 'unauthorized', status: 401, code: err.code, message: err.message }
  }
  if (err.status === 403 || err.code === 'auth.password_change_required') {
    return { kind: 'forbidden', status: 403, code: err.code, message: err.message }
  }
  if (err.status === 404 || err.code === 'not_found') {
    return { kind: 'not_found', status: 404, code: err.code, message: err.message }
  }
  if (err.status === 429) {
    return { kind: 'rate_limited', status: 429, code: err.code, message: err.message }
  }
  if (err.status === 0) {
    return { kind: 'network', status: 0, code: err.code, message: err.message }
  }
  return { kind: 'unknown', status: err.status, code: err.code, message: err.message }
}

function isApprovalExpired(approval: ApprovalView, now: number): boolean {
  if (!approval.expires_at) return false
  const expires = Date.parse(approval.expires_at)
  if (Number.isNaN(expires)) return false
  return expires <= now
}

// ── Hook ────────────────────────────────────────────────────────────────

export interface UseChatStreamResult {
  session: ChatSession | null
  messages: MessageView[]
  /** 正在流式接收的 assistant 消息（没有则为 null）。 */
  live: MessageView | null
  running: boolean
  approvals: ApprovalView[]
  loading: boolean
  error: ChatErrorState | null
  /** 正在提交决策的审批 id（弹窗按钮 pending 用）。 */
  decidingId: string | null
  send: (text: string) => Promise<void>
  steer: (text: string) => Promise<void>
  stop: () => void
  decide: (approvalId: string, decision: ApprovalDecision, note?: string) => Promise<void>
  reload: () => void
  rename: (title: string) => Promise<void>
  setModel: (model: string | null) => Promise<void>
}

export function useChatStream(sessionId: string): UseChatStreamResult {
  const router = useRouter()
  const { t } = useI18n()

  const [session, setSession] = useState<ChatSession | null>(null)
  const [messages, setMessages] = useState<MessageView[]>([])
  const [running, setRunning] = useState(false)
  const [approvals, setApprovals] = useState<ApprovalView[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<ChatErrorState | null>(null)
  const [decidingId, setDecidingId] = useState<string | null>(null)

  const mountedRef = useRef(true)
  const runningRef = useRef(false)
  const activeRunIdRef = useRef<string | null>(null)
  const activeAssistantIdRef = useRef<string | null>(null)
  const streamStartedRef = useRef<Set<string>>(new Set())
  const textBufferRef = useRef<Map<string, string>>(new Map())
  const reasoningBufferRef = useRef<Map<string, string>>(new Map())
  const rafRef = useRef<number | null>(null)
  /**
   * 当前渲染对应的会话 id。异步请求（load / resync / send / decide …）返回后
   * 用它判断结果是否仍属于当前会话：切换会话时旧请求的响应会被丢弃，
   * 避免「旧会话数据覆盖新会话」的竞态。
   */
  const sessionKeyRef = useRef(sessionId)
  sessionKeyRef.current = sessionId

  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  // 会话切换：清空所有「本次订阅」的临时状态
  useEffect(() => {
    setSession(null)
    setMessages([])
    setApprovals([])
    setError(null)
    setLoading(true)
    activeRunIdRef.current = null
    activeAssistantIdRef.current = null
    runningRef.current = false
    streamStartedRef.current = new Set()
    textBufferRef.current = new Map()
    reasoningBufferRef.current = new Map()
    // 旧会话遗留的合批回调必须取消，否则会在新会话上再跑一次空 flush
    if (rafRef.current !== null && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafRef.current)
    }
    rafRef.current = null
  }, [sessionId])

  // 卸载时取消未执行的 rAF
  useEffect(() => {
    return () => {
      if (rafRef.current !== null && typeof cancelAnimationFrame === 'function') {
        cancelAnimationFrame(rafRef.current)
      }
      rafRef.current = null
    }
  }, [])

  const setRunningBoth = useCallback((value: boolean) => {
    runningRef.current = value
    setRunning(value)
  }, [])

  const flushNow = useCallback(() => {
    if (rafRef.current !== null && typeof cancelAnimationFrame === 'function') {
      cancelAnimationFrame(rafRef.current)
    }
    rafRef.current = null

    const textEntries = Array.from(textBufferRef.current.entries())
    const reasoningEntries = Array.from(reasoningBufferRef.current.entries())
    if (textEntries.length === 0 && reasoningEntries.length === 0) return
    textBufferRef.current.clear()
    reasoningBufferRef.current.clear()

    setMessages((prev) => {
      let next = prev
      for (const [id, delta] of reasoningEntries) {
        next = next.map((item) => (item.id === id ? { ...item, reasoning: item.reasoning + delta } : item))
      }
      for (const [id, delta] of textEntries) {
        next = next.map((item) =>
          item.id === id ? { ...item, text: item.text + delta, streaming: true } : item,
        )
      }
      return next
    })
  }, [])

  const scheduleFlush = useCallback(() => {
    if (rafRef.current !== null) return
    if (typeof window === 'undefined' || typeof window.requestAnimationFrame !== 'function') {
      flushNow()
      return
    }
    rafRef.current = window.requestAnimationFrame(() => {
      rafRef.current = null
      flushNow()
    })
  }, [flushNow])

  const finishStreaming = useCallback(() => {
    flushNow()
    setMessages((prev) =>
      prev.some((item) => item.streaming)
        ? prev.map((item) => (item.streaming ? { ...item, streaming: false } : item))
        : prev,
    )
  }, [flushNow])

  /** 确保存在「当前 assistant 消息」；没有则建一个占位（推理先行时会用到）。 */
  const ensureAssistant = useCallback((): string => {
    if (activeAssistantIdRef.current) return activeAssistantIdRef.current
    const id = `stream:${activeRunIdRef.current ?? 'pending'}`
    activeAssistantIdRef.current = id
    setMessages((prev) => (prev.some((item) => item.id === id) ? prev : [...prev, newAssistantView(id)]))
    return id
  }, [])

  /** `message.delta` 的落点：占位消息改名为真实 id，否则按需新建。 */
  const adoptMessage = useCallback((messageId: string) => {
    const active = activeAssistantIdRef.current
    if (active === messageId) return
    const adoptable = active === null || active.startsWith('stream:')
    if (adoptable && active) {
      // 占位消息改名时把尚未 flush 的推理文本一并转过去
      const buffered = reasoningBufferRef.current.get(active)
      if (buffered !== undefined) {
        reasoningBufferRef.current.delete(active)
        reasoningBufferRef.current.set(
          messageId,
          (reasoningBufferRef.current.get(messageId) ?? '') + buffered,
        )
      }
    }
    activeAssistantIdRef.current = messageId
    setMessages((prev) => adoptViewId(prev, adoptable ? (active ?? '') : '', messageId))
  }, [])

  const applyHistory = useCallback(
    (incoming: ChatMessage[]) => {
      const fallbackName = t('chat.tool.fallbackName')
      const views = incoming.map((message) => toView(message, fallbackName))
      setMessages((prev) => {
        const byId = new Map(prev.map((item) => [item.id, item] as const))
        for (const view of views) {
          const current = byId.get(view.id)
          if (!current) {
            byId.set(view.id, view)
            continue
          }
          if (current.streaming) {
            // 流式中的消息：文本一般以 delta 为准，但 `run.gap` 后快照可能比
            // 本地累积更完整（丢了中间 delta），此时以更长的快照文本为准。
            byId.set(view.id, {
              ...current,
              kind: view.kind || current.kind,
              text: view.text.length > current.text.length ? view.text : current.text,
              tool_calls: mergeToolLists(current.tool_calls, view.tool_calls),
            })
            continue
          }
          byId.set(view.id, {
            ...view,
            text: view.text || current.text,
            reasoning: current.reasoning || view.reasoning,
            tool_calls: mergeToolLists(view.tool_calls, current.tool_calls),
          })
        }
        return sortByCreatedAt(Array.from(byId.values()))
      })
    },
    [t],
  )

  const load = useCallback(async () => {
    const requested = sessionId
    const isCurrent = () => mountedRef.current && sessionKeyRef.current === requested
    setLoading(true)
    setError(null)
    try {
      const [detail, list] = await Promise.all([
        api.getSession(requested),
        api.listMessages(requested, { limit: HISTORY_LIMIT }),
      ])
      // 期间切换了会话：旧响应直接丢弃，避免串到新会话
      if (!isCurrent()) return
      setSession(detail.session)
      applyHistory(list.messages)
      if (detail.active_run) {
        activeRunIdRef.current = detail.active_run.run_id
        setRunningBoth(true)
      } else {
        activeRunIdRef.current = null
        setRunningBoth(false)
      }
    } catch (caught) {
      if (!isCurrent()) return
      const state = classifyError(caught)
      if (state.kind === 'unauthorized') {
        router.replace(`/login?next=${encodeURIComponent(`/chat/${requested}`)}`)
      } else if (state.kind === 'forbidden') {
        router.replace('/change-password')
      }
      setError(state)
    } finally {
      if (isCurrent()) setLoading(false)
    }
  }, [sessionId, applyHistory, setRunningBoth, router])

  useEffect(() => {
    void load()
  }, [load])

  const resync = useCallback(async () => {
    const requested = sessionId
    const isCurrent = () => mountedRef.current && sessionKeyRef.current === requested
    const list = await api.listMessages(requested, { limit: HISTORY_LIMIT })
    if (!isCurrent()) return
    applyHistory(list.messages)
    const runId = activeRunIdRef.current
    if (!runId) return
    try {
      const detail = await api.getRun(runId)
      if (!isCurrent()) return
      const active = isActiveRunStatus(detail.run.status)
      setRunningBoth(active)
      if (!active) finishStreaming()
    } catch {
      // run 记录可能已被清理：保持当前状态，等 run.* 事件兜底
    }
  }, [sessionId, applyHistory, setRunningBoth, finishStreaming])

  const applyEvent = useCallback(
    (event: SessionEvent) => {
      switch (event.type) {
        case 'run.started': {
          if (event.run_id) activeRunIdRef.current = event.run_id
          setRunningBoth(true)
          return
        }
        case 'message.delta': {
          const { message_id, delta } = event.data
          if (!message_id || !delta) return
          adoptMessage(message_id)
          if (!streamStartedRef.current.has(message_id)) {
            streamStartedRef.current.add(message_id)
            // 回放场景：历史快照里已有该消息的终稿，从头累积文本避免双份
            setMessages((prev) =>
              prev.map((item) =>
                item.id === message_id && item.text && !item.streaming
                  ? { ...item, text: '', reasoning: '', streaming: true }
                  : item,
              ),
            )
          }
          textBufferRef.current.set(message_id, (textBufferRef.current.get(message_id) ?? '') + delta)
          scheduleFlush()
          return
        }
        case 'reasoning.delta': {
          const { delta } = event.data
          if (!delta) return
          const id = ensureAssistant()
          reasoningBufferRef.current.set(id, (reasoningBufferRef.current.get(id) ?? '') + delta)
          scheduleFlush()
          return
        }
        case 'message.completed': {
          const data = event.data
          const id = data.message_id
          if (!id) return
          adoptMessage(id)
          textBufferRef.current.delete(id)
          reasoningBufferRef.current.delete(id)
          const finalText = textFromContent(data.content)
          const fallbackName = t('chat.tool.fallbackName')
          // 服务端 `tool_calls` 只发数量（详细调用走 tool.call/tool.result 事件）。
          const finalCalls = (Array.isArray(data.tool_calls) ? data.tool_calls : []).map(
            (call, index) => toToolView(call, index, fallbackName),
          )
          setMessages((prev) =>
            prev.map((item) =>
              item.id === id
                ? {
                    ...item,
                    text: finalText || item.text,
                    tool_calls: mergeToolLists(item.tool_calls, finalCalls),
                    streaming: false,
                  }
                : item,
            ),
          )
          return
        }
        case 'tool.call': {
          const data = event.data
          const id = ensureAssistant()
          const view: ToolCallView = {
            tool_call_id: data.tool_call_id,
            name: data.name,
            args: data.args,
            status: 'running',
            result: undefined,
            error: null,
          }
          setMessages((prev) =>
            prev.map((item) =>
              item.id === id ? { ...item, tool_calls: upsertToolCall(item.tool_calls, view) } : item,
            ),
          )
          return
        }
        case 'tool.result': {
          const data = event.data
          const status = toolStatusFrom(data.status)
          // `tool.result` 可能先于 `tool.call` 到达（或重连只补到结果）：
          // 没有同名卡片时按当前 assistant 消息补建一张，避免结果丢失。
          const ownerId = activeAssistantIdRef.current ?? ensureAssistant()
          const fallback: ToolCallView = {
            tool_call_id: data.tool_call_id,
            name: t('chat.tool.fallbackName'),
            args: undefined,
            status,
            result: data.result,
            error: data.error ?? null,
          }
          setMessages((prev) => {
            let touched = false
            const next = prev.map((item) => {
              if (!item.tool_calls.some((call) => call.tool_call_id === data.tool_call_id)) return item
              touched = true
              return {
                ...item,
                tool_calls: item.tool_calls.map((call) =>
                  call.tool_call_id === data.tool_call_id
                    ? { ...call, status, result: data.result, error: data.error ?? null }
                    : call,
                ),
              }
            })
            if (touched) return next
            return next.map((item) =>
              item.id === ownerId
                ? { ...item, tool_calls: upsertToolCall(item.tool_calls, fallback) }
                : item,
            )
          })
          return
        }
        case 'approval.required': {
          const data = event.data
          setApprovals((prev) =>
            prev.some((item) => item.approval_id === data.approval_id)
              ? prev
              : [...prev, { ...data, receivedAt: Date.now() }],
          )
          return
        }
        case 'approval.resolved': {
          const { approval_id } = event.data
          setApprovals((prev) => prev.filter((item) => item.approval_id !== approval_id))
          return
        }
        case 'run.completed': {
          activeRunIdRef.current = null
          activeAssistantIdRef.current = null
          setRunningBoth(false)
          // 运行结束：残留的待审批项已经随 run 失效，直接出队
          setApprovals((prev) => (prev.length === 0 ? prev : []))
          finishStreaming()
          return
        }
        case 'run.failed': {
          const { code, message } = event.data
          activeRunIdRef.current = null
          activeAssistantIdRef.current = null
          setRunningBoth(false)
          setApprovals((prev) => (prev.length === 0 ? prev : []))
          finishStreaming()
          toast.danger(
            code
              ? t('chat.errors.runFailedWithCode', {
                  code,
                  message: message || t('chat.errors.unknown'),
                })
              : t('chat.errors.runFailed', { message: message || t('chat.errors.unknown') }),
          )
          return
        }
        case 'run.cancelled': {
          activeRunIdRef.current = null
          activeAssistantIdRef.current = null
          setRunningBoth(false)
          setApprovals((prev) => (prev.length === 0 ? prev : []))
          finishStreaming()
          return
        }
        default:
          return
      }
    },
    [adoptMessage, ensureAssistant, finishStreaming, scheduleFlush, setRunningBoth, t],
  )

  useEffect(() => {
    if (!sessionId) return
    const unsubscribe = subscribeSessionEvents(
      sessionId,
      {
        onEvent: (event) => {
          // 旧订阅在清理前可能还会投递事件：只处理当前会话的
          if (sessionKeyRef.current !== sessionId) return
          applyEvent(event)
        },
        onGap: () => {
          if (sessionKeyRef.current !== sessionId) return
          void resync().catch(() => {
            // gap 重拉失败不打断页面，等下一次事件或手动重试
          })
        },
        onError: (err) => {
          if (err.status === 401) {
            router.replace(`/login?next=${encodeURIComponent(`/chat/${sessionId}`)}`)
          }
        },
      },
      { pauseWhenHidden: true },
    )
    return () => {
      unsubscribe()
    }
  }, [sessionId, applyEvent, resync, router])

  // 审批过期自动出队
  useEffect(() => {
    if (approvals.length === 0) return
    const timer = window.setInterval(() => {
      const now = Date.now()
      setApprovals((prev) => {
        const next = prev.filter((item) => !isApprovalExpired(item, now))
        return next.length === prev.length ? prev : next
      })
    }, APPROVAL_SWEEP_MS)
    return () => window.clearInterval(timer)
  }, [approvals.length])

  const send = useCallback(
    async (raw: string) => {
      const text = raw.trim()
      if (!text) throw new ApiError(400, 'empty_message', t('chat.errors.emptyMessage'))

      const requested = sessionId
      const isCurrent = () => mountedRef.current && sessionKeyRef.current === requested
      const localId = makeLocalId('local')
      const optimistic: MessageView = {
        id: localId,
        role: 'user',
        kind: 'chat',
        text,
        reasoning: '',
        tool_calls: [],
        created_at: nowIso(),
        streaming: false,
        pending: true,
        failed: false,
      }
      setMessages((prev) => [...prev, optimistic])

      try {
        const result = await api.sendMessage(requested, text, {
          model: session?.model ?? undefined,
        })
        if (!isCurrent()) return
        setMessages((prev) => promoteLocalView(prev, localId, result.message_id))
        activeRunIdRef.current = result.run_id
        setRunningBoth(true)
      } catch (caught) {
        if (isCurrent()) {
          setMessages((prev) => prev.filter((item) => item.id !== localId))
        }
        const err = ApiError.from(caught)
        if (err.status === 401) {
          router.replace(`/login?next=${encodeURIComponent(`/chat/${requested}`)}`)
          throw err
        }
        // 用户动作失败只在 Composer 提示一次（避免 hook + Composer 双 toast），
        // 这里只把错误整理成可读文案再抛。
        if (err.status === 409) {
          throw new ApiError(
            err.status,
            err.code,
            err.code === 'run.busy'
              ? t('chat.errors.runBusy')
              : err.message || t('chat.errors.sendRejected'),
          )
        }
        if (err.status === 429) {
          // 配额耗尽：后端会在 message 里带可读的 Retry-After 提示
          throw new ApiError(
            err.status,
            err.code,
            err.code === 'quota_exceeded'
              ? t('chat.errors.quotaExceeded')
              : err.message || t('chat.errors.rateLimited'),
          )
        }
        throw err
      }
    },
    [sessionId, session?.model, setRunningBoth, router, t],
  )

  const steer = useCallback(
    async (raw: string) => {
      const text = raw.trim()
      if (!text) return
      if (!runningRef.current) {
        await send(text)
        return
      }
      const requested = sessionId
      const isCurrent = () => mountedRef.current && sessionKeyRef.current === requested
      try {
        await api.steer(requested, text)
        if (isCurrent()) toast.success(t('chat.errors.steered'))
        void resync().catch(() => {
          // 插话后的消息快照刷新失败不影响主流程
        })
      } catch (caught) {
        const err = ApiError.from(caught)
        if (err.status === 409 || err.code === 'no_active_run') {
          // 已经空闲：按普通发送处理
          await send(text)
          return
        }
        throw err
      }
    },
    [sessionId, send, resync, t],
  )

  const stop = useCallback(() => {
    const requested = sessionId
    void (async () => {
      try {
        const result = await api.interrupt(requested)
        if (!mountedRef.current || sessionKeyRef.current !== requested) return
        if (result.ok) {
          setRunningBoth(false)
          finishStreaming()
          toast.info(t('chat.errors.stopRequested'))
        } else {
          toast.info(t('chat.errors.noActiveRun'))
        }
      } catch (caught) {
        if (!mountedRef.current || sessionKeyRef.current !== requested) return
        toast.danger(ApiError.from(caught).message || t('chat.errors.stopFailed'))
      }
    })()
  }, [sessionId, setRunningBoth, finishStreaming, t])

  const decide = useCallback(
    async (approvalId: string, decision: ApprovalDecision, note?: string) => {
      const requested = sessionId
      const isCurrent = () => mountedRef.current && sessionKeyRef.current === requested
      setDecidingId(approvalId)
      try {
        await api.resolveApproval(requested, approvalId, decision, note)
        if (!isCurrent()) return
        setApprovals((prev) => prev.filter((item) => item.approval_id !== approvalId))
        toast.success(
          decision === 'allow' ? t('chat.approval.allowed') : t('chat.approval.denied'),
        )
      } catch (caught) {
        const err = ApiError.from(caught)
        if (!isCurrent()) throw err
        toast.danger(err.message || t('chat.errors.approvalFailed'))
        if (err.status === 409 || err.status === 404 || err.code === 'approval_not_pending') {
          // 审批已失效：直接出队，避免卡住后续审批
          setApprovals((prev) => prev.filter((item) => item.approval_id !== approvalId))
        }
        throw err
      } finally {
        if (isCurrent()) setDecidingId(null)
      }
    },
    [sessionId, t],
  )

  const reload = useCallback(() => {
    void load()
  }, [load])

  const rename = useCallback(
    async (title: string) => {
      const trimmed = title.trim()
      if (!trimmed) return
      const requested = sessionId
      try {
        const result = await api.updateSession(requested, { title: trimmed })
        if (mountedRef.current && sessionKeyRef.current === requested) setSession(result.session)
      } catch (caught) {
        if (mountedRef.current && sessionKeyRef.current === requested) {
          toast.danger(ApiError.from(caught).message || t('chat.errors.renameFailed'))
        }
        throw caught
      }
    },
    [sessionId, t],
  )

  const setModel = useCallback(
    async (model: string | null) => {
      const requested = sessionId
      try {
        const result = await api.updateSession(requested, { model: model ?? '' })
        if (mountedRef.current && sessionKeyRef.current === requested) setSession(result.session)
      } catch (caught) {
        if (mountedRef.current && sessionKeyRef.current === requested) {
          toast.danger(ApiError.from(caught).message || t('chat.errors.modelFailed'))
        }
        throw caught
      }
    },
    [sessionId, t],
  )

  const live = useMemo(() => messages.find((item) => item.streaming) ?? null, [messages])

  return {
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
  }
}
