/**
 * 本地联调反代：`/api/v1/*` → `CHIRON_API_ORIGIN`（默认 http://127.0.0.1:8088）。
 *
 * 为什么需要它：Rust 服务按 `Host` 解析租户，浏览器同时访问前台域名与租户域名
 * 会触发跨域与 cookie 作用域问题。这里让前端同源调用 `/api/v1`，由服务端补上
 * `Host: CHIRON_DEV_TENANT_HOST` 后转发，cookie 与 SSE 都留在同源下。
 *
 * 注意：
 * - 只转发租户 API（`/api/v1/...`），平台 API `/api/platform/v1` 不在此路由下；
 * - SSE 响应必须原样流式转发：用 `node:http` 拿到 IncomingMessage 后直接
 *   `Readable.toWeb`，不读取、不缓冲、不压缩；
 * - `Set-Cookie` 多条要逐条 append，不能合并；
 * - 反代自身产生的错误体统一用英文 + 稳定 code（`not_found` / `proxy_unavailable` /
 *   `proxy_failed`），不透出上游地址或 Node 原始错误细节，避免中英混杂与信息泄露。
 */

import { request as httpRequest, type IncomingHttpHeaders, type RequestOptions } from 'node:http'
import { request as httpsRequest } from 'node:https'
import { Readable } from 'node:stream'
import type { NextRequest } from 'next/server'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'
export const fetchCache = 'force-no-store'

const HOP_BY_HOP = new Set([
  'connection',
  'keep-alive',
  'proxy-authenticate',
  'proxy-authorization',
  'te',
  'trailer',
  'transfer-encoding',
  'upgrade',
])

function upstreamTarget(): { origin: URL } {
  const rawOrigin = process.env.CHIRON_API_ORIGIN || 'http://127.0.0.1:8088'
  let origin: URL
  try {
    origin = new URL(rawOrigin)
  } catch {
    origin = new URL('http://127.0.0.1:8088')
  }
  return { origin }
}

// 开发时浏览器在 localhost，必须显式指定租户 Host；生产必须沿用请求的真实 Host，
// 否则多租户前台会把所有租户请求固定到同一个开发租户。
function upstreamHost(request: NextRequest, incoming: URL): string {
  if (process.env.NODE_ENV !== 'production') {
    return process.env.CHIRON_DEV_TENANT_HOST || incoming.host
  }
  return request.headers.get('host') || incoming.host
}

function buildUpstreamHeaders(request: NextRequest, tenantHost: string): Record<string, string> {
  const headers: Record<string, string> = {}
  request.headers.forEach((value, key) => {
    const lower = key.toLowerCase()
    if (
      lower === 'host' ||
      lower === 'forwarded' ||
      lower === 'x-forwarded-host' ||
      lower === 'x-real-ip' ||
      HOP_BY_HOP.has(lower)
    ) {
      return
    }
    headers[lower] = value
  })
  // X-Forwarded-For 由前置 Caddy 覆盖；这里只透传该单一来源，后端据此做登录限流。
  headers['x-forwarded-for'] =
    request.headers.get('x-forwarded-for') ||
    request.headers.get('x-real-ip') ||
    ''
  // 强制租户 Host：Rust 侧按它解析租户并绑定连接池
  headers.host = tenantHost
  return headers
}

function toResponseHeaders(upstream: IncomingHttpHeaders): Headers {
  const headers = new Headers()
  for (const [key, value] of Object.entries(upstream)) {
    if (!value) continue
    const lower = key.toLowerCase()
    if (HOP_BY_HOP.has(lower)) continue
    if (Array.isArray(value)) {
      for (const item of value) headers.append(lower, item)
    } else {
      headers.set(lower, value)
    }
  }
  // 明确告诉中间层不要缓存/缓冲流式响应
  headers.set('cache-control', 'no-cache, no-transform')
  headers.set('x-accel-buffering', 'no')
  return headers
}

async function proxy(request: NextRequest): Promise<Response> {
  const { origin } = upstreamTarget()
  const incoming = new URL(request.url)
  const host = upstreamHost(request, incoming)
  if (!incoming.pathname.startsWith('/api/v1/') || !host) {
    return Response.json(
      { error: { code: 'not_found', message: 'unsupported path' } },
      { status: 404 },
    )
  }

  const target: RequestOptions = {
    protocol: origin.protocol,
    hostname: origin.hostname,
    port: origin.port || (origin.protocol === 'https:' ? 443 : 80),
    method: request.method,
    path: `${incoming.pathname}${incoming.search}`,
    headers: buildUpstreamHeaders(request, host),
  }

  const sender = origin.protocol === 'https:' ? httpsRequest : httpRequest

  try {
    const upstreamResponse = await new Promise<Response>((resolve, reject) => {
      const upstream = sender(target, (response) => {
        const status = response.statusCode ?? 502
        const headers = toResponseHeaders(response.headers)
        if (status === 204 || status === 205 || status === 304) {
          response.resume()
          resolve(new Response(null, { status, statusText: response.statusMessage, headers }))
          return
        }
        // 原样透传字节流：SSE 的 text/event-stream 不被缓冲
        const body = Readable.toWeb(response) as unknown as ReadableStream<Uint8Array>
        resolve(
          new Response(body, {
            status,
            statusText: response.statusMessage,
            headers,
          }),
        )
      })

      upstream.on('error', (error) => {
        // 只记录服务端日志；返回给浏览器的错误体保持通用英文，不泄露上游地址。
        console.error(`[v1-proxy] upstream error for ${incoming.pathname}:`, error.message)
        reject(new Response(
          JSON.stringify({
            error: {
              code: 'proxy_unavailable',
              message: 'upstream service is unavailable',
            },
          }),
          { status: 502, headers: { 'content-type': 'application/json; charset=utf-8' } },
        ))
      })

      // 长连接（SSE）不设超时；普通请求 30s 兜底。客户端断开时立刻释放上游连接。
      const isStream = incoming.pathname.endsWith('/events')
      upstream.setTimeout(
        isStream ? 0 : 30_000,
        () => upstream.destroy(new Error('upstream request timed out')),
      )
      if (request.signal) {
        request.signal.addEventListener('abort', () => upstream.destroy(), { once: true })
      }

      if (request.body) {
        const nodeBody = Readable.fromWeb(
          request.body as unknown as Parameters<typeof Readable.fromWeb>[0],
        )
        nodeBody.on('error', () => upstream.destroy()).pipe(upstream)
      } else {
        upstream.end()
      }
    })

    return upstreamResponse
  } catch (error) {
    if (error instanceof Response) return error
    console.error(`[v1-proxy] request failed for ${incoming.pathname}:`, error)
    return Response.json(
      {
        error: {
          code: 'proxy_failed',
          message: 'upstream request failed',
        },
      },
      { status: 502 },
    )
  }
}

export {
  proxy as GET,
  proxy as POST,
  proxy as PUT,
  proxy as PATCH,
  proxy as DELETE,
  proxy as OPTIONS,
  proxy as HEAD,
}
