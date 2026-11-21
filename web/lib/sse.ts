/**
 * SSE 客户端。
 *
 * - 真实模式：`fetch` + ReadableStream 解析 `text/event-stream`（`id:` / `event:` /
 *   `data:` 多行 / 空行分隔 / `:` 注释），带 `Last-Event-ID` 指数退避重连（上限 10s），
 *   按 envelope `id` 去重；收到 `run.gap` 时额外回调 `onGap`，上层应重拉消息快照。
 * - mock 模式：转给 `lib/mock.ts` 的 `subscribeMock`。
 *
 * 页面只依赖 `subscribeSessionEvents`，不直接接触传输层。
 */

import { USE_MOCK, apiBase, getToken } from './api'
import { subscribeMock } from './mock'
import { uiText } from './ui-locale'
import { ApiError, type SessionEvent, type SessionEventHandlers, type SubscribeOptions, type Unsubscribe } from './types'

export { subscribeMock }

const MAX_BACKOFF_MS = 10_000
const BASE_BACKOFF_MS = 1_000

interface SseMessage {
  id: string
  event: string
  data: string
}

/** 增量 SSE 解析器：喂 chunk，吐完整事件。 */
class SseParser {
  private buffer = ''
  private id = ''
  private event = ''
  private dataLines: string[] = []

  push(chunk: string): SseMessage[] {
    this.buffer += chunk
    const messages: SseMessage[] = []
    for (const line of this.extractLines()) {
      if (line === '') {
        const message = this.flush()
        if (message) messages.push(message)
        continue
      }
      if (line.startsWith(':')) continue
      const colon = line.indexOf(':')
      const field = colon === -1 ? line : line.slice(0, colon)
      let value = colon === -1 ? '' : line.slice(colon + 1)
      if (value.startsWith(' ')) value = value.slice(1)
      switch (field) {
        case 'id':
          this.id = value
          break
        case 'event':
          this.event = value
          break
        case 'data':
          this.dataLines.push(value)
          break
        default:
          break
      }
    }
    return messages
  }

  /**
   * 按 `\r\n` / `\n` / `\r` 切出完整行；不完整的尾行留在 buffer 里，
   * `\r\n` 被网络分块拆开时也不会提前 flush。
   */
  private extractLines(): string[] {
    const lines: string[] = []
    for (;;) {
      const cr = this.buffer.indexOf('\r')
      const lf = this.buffer.indexOf('\n')
      if (cr === -1 && lf === -1) break

      if (cr !== -1 && (lf === -1 || cr < lf)) {
        // 末尾的孤立 \r 可能是 \r\n 的前半，等下一块数据
        if (cr === this.buffer.length - 1) break
        lines.push(this.buffer.slice(0, cr))
        this.buffer = this.buffer.slice(this.buffer[cr + 1] === '\n' ? cr + 2 : cr + 1)
        continue
      }

      lines.push(this.buffer.slice(0, lf))
      this.buffer = this.buffer.slice(lf + 1)
    }
    return lines
  }

  private flush(): SseMessage | null {
    if (this.dataLines.length === 0) {
      this.id = ''
      this.event = ''
      return null
    }
    const message: SseMessage = {
      id: this.id,
      event: this.event,
      data: this.dataLines.join('\n'),
    }
    this.id = ''
    this.event = ''
    this.dataLines = []
    return message
  }
}

function parseEvent(message: SseMessage): SessionEvent | null {
  try {
    const raw = JSON.parse(message.data) as Partial<SessionEvent> & { type?: string }
    if (!raw || typeof raw !== 'object' || typeof raw.type !== 'string') return null
    return {
      id: raw.id || message.id || `sse_${raw.seq ?? Date.now()}`,
      seq: typeof raw.seq === 'number' ? raw.seq : Number(message.id) || 0,
      run_id: raw.run_id ?? null,
      type: raw.type,
      data: raw.data ?? {},
      ts: raw.ts,
    } as SessionEvent
  } catch {
    return null
  }
}

function backoffDelay(attempt: number): number {
  const base = Math.min(BASE_BACKOFF_MS * 2 ** Math.max(0, attempt - 1), MAX_BACKOFF_MS)
  return base + Math.random() * 250
}

function waitForVisible(signal: AbortSignal): Promise<void> {
  if (typeof document === 'undefined' || !document.hidden) return Promise.resolve()
  return new Promise((resolve) => {
    const cleanup = () => {
      document.removeEventListener('visibilitychange', onVisible)
      signal.removeEventListener('abort', done)
    }
    const done = () => {
      cleanup()
      resolve()
    }
    const onVisible = () => {
      if (document.hidden) return
      done()
    }
    document.addEventListener('visibilitychange', onVisible)
    // 页面隐藏期间被取消订阅：立刻结束等待，避免监听器泄漏
    signal.addEventListener('abort', done, { once: true })
  })
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(done, ms)
    function done() {
      clearTimeout(timer)
      signal.removeEventListener('abort', done)
      resolve()
    }
    signal.addEventListener('abort', done, { once: true })
  })
}

/**
 * 订阅某个会话的事件流。
 *
 * @example
 * const stop = subscribeSessionEvents(id, { onEvent, onGap }, { lastEventId: 42 })
 * // 组件卸载时 stop()
 */
export function subscribeSessionEvents(
  sessionId: string,
  handlers: SessionEventHandlers,
  options: SubscribeOptions = {},
): Unsubscribe {
  if (USE_MOCK) return subscribeMockSession(sessionId, handlers)
  return subscribeReal(sessionId, handlers, options)
}

function subscribeMockSession(sessionId: string, handlers: SessionEventHandlers): Unsubscribe {
  const seen = new Set<string>()
  queueMicrotask(() => handlers.onOpen?.())
  return subscribeMock(sessionId, (event) => {
    if (event.id && seen.has(event.id)) return
    if (event.id) seen.add(event.id)
    handlers.onEvent(event)
    if (event.type === 'run.gap') handlers.onGap?.()
  })
}

function subscribeReal(
  sessionId: string,
  handlers: SessionEventHandlers,
  options: SubscribeOptions,
): Unsubscribe {
  const controller = new AbortController()
  const seen = new Set<string>()
  let lastSeq = options.lastEventId == null ? 0 : Number(options.lastEventId) || 0
  let attempt = 0
  let stopped = false

  async function run(): Promise<void> {
    while (!stopped) {
      if (options.pauseWhenHidden) await waitForVisible(controller.signal)
      if (stopped) return

      const headers: Record<string, string> = { Accept: 'text/event-stream' }
      if (lastSeq > 0) headers['Last-Event-ID'] = String(lastSeq)
      const token = getToken()
      if (token) headers.Authorization = `Bearer ${token}`

      let response: Response
      try {
        response = await fetch(`${apiBase()}/api/v1/sessions/${encodeURIComponent(sessionId)}/events`, {
          method: 'GET',
          headers,
          credentials: 'include',
          cache: 'no-store',
          signal: controller.signal,
        })
      } catch (error) {
        if (stopped || controller.signal.aborted) return
        attempt += 1
        handlers.onError?.(
          ApiError.from(error, uiText('事件流连接失败', 'Event stream connection failed')),
        )
        await sleep(backoffDelay(attempt), controller.signal)
        continue
      }

      if (!response.ok) {
        const status = response.status
        let code = `http_${status}`
        let message = uiText('事件流连接失败', 'Event stream connection failed')
        try {
          const body = (await response.json()) as { error?: { code?: string; message?: string } }
          if (body?.error) {
            code = body.error.code || code
            message = body.error.message || message
          }
        } catch {
          // 忽略非 JSON 错误体
        }
        handlers.onError?.(new ApiError(status, code, message))
        // 认证/权限/资源不存在：不再重试
        if (status === 401 || status === 403 || status === 404) return
        attempt += 1
        await sleep(backoffDelay(attempt), controller.signal)
        continue
      }

      handlers.onOpen?.()
      attempt = 0

      const reader = response.body?.getReader()
      if (!reader) {
        attempt += 1
        await sleep(backoffDelay(attempt), controller.signal)
        continue
      }

      const parser = new SseParser()
      const decoder = new TextDecoder()
      try {
        for (;;) {
          const { value, done } = await reader.read()
          if (done) break
          const events = parser.push(decoder.decode(value, { stream: true }))
          for (const message of events) deliver(message)
          if (stopped) return
        }
      } catch (error) {
        if (stopped || controller.signal.aborted) return
        handlers.onError?.(ApiError.from(error, uiText('事件流中断', 'Event stream interrupted')))
      } finally {
        try {
          reader.releaseLock()
        } catch {
          // 已释放
        }
      }

      if (stopped) return
      attempt += 1
      await sleep(backoffDelay(attempt), controller.signal)
    }
  }

  function deliver(message: SseMessage): void {
    const event = parseEvent(message)
    if (!event) return
    if (seen.has(event.id)) return
    seen.add(event.id)
    if (event.seq > lastSeq) lastSeq = event.seq
    handlers.onEvent(event)
    if (event.type === 'run.gap') handlers.onGap?.()
  }

  void run()

  return () => {
    if (stopped) return
    stopped = true
    controller.abort()
  }
}
