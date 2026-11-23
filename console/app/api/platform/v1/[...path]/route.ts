/**
 * 仅开发联调：把控制台同源 /api/platform/v1/* 转发到本地 chiron-server。
 * 生产必须由反代处理；未显式设置 CHIRON_DEV_PROXY=1 时返回 404。
 */
import { request as httpRequest, type IncomingHttpHeaders, type RequestOptions } from 'node:http'
import { request as httpsRequest } from 'node:https'
import { Readable } from 'node:stream'
import type { NextRequest } from 'next/server'

export const runtime = 'nodejs'
export const dynamic = 'force-dynamic'

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

function upstream(): { origin: URL } | null {
  if (process.env.CHIRON_DEV_PROXY !== '1') return null
  try {
    return { origin: new URL(process.env.CHIRON_API_ORIGIN || 'http://127.0.0.1:8088') }
  } catch {
    return null
  }
}

function upstreamHeaders(request: NextRequest, host: string): Record<string, string> {
  const headers: Record<string, string> = {}
  request.headers.forEach((value, key) => {
    if (key.toLowerCase() === 'host' || HOP_BY_HOP.has(key.toLowerCase())) return
    headers[key.toLowerCase()] = value
  })
  headers.host = host
  return headers
}

function responseHeaders(upstreamHeaders: IncomingHttpHeaders): Headers {
  const headers = new Headers()
  for (const [key, value] of Object.entries(upstreamHeaders)) {
    if (!value || HOP_BY_HOP.has(key.toLowerCase())) continue
    if (Array.isArray(value)) for (const item of value) headers.append(key, item)
    else headers.set(key, value)
  }
  headers.set('cache-control', 'no-store')
  return headers
}

async function proxy(request: NextRequest): Promise<Response> {
  const target = upstream()
  const incoming = new URL(request.url)
  if (!target || !incoming.pathname.startsWith('/api/platform/v1/')) {
    return Response.json({ error: { code: 'not_found', message: 'unsupported path' } }, { status: 404 })
  }
  const origin = target.origin
  const options: RequestOptions = {
    protocol: origin.protocol,
    hostname: origin.hostname,
    port: origin.port || (origin.protocol === 'https:' ? 443 : 80),
    method: request.method,
    path: `${incoming.pathname}${incoming.search}`,
    headers: upstreamHeaders(request, origin.host),
  }
  const sender = origin.protocol === 'https:' ? httpsRequest : httpRequest
  try {
    const response = await new Promise<Response>((resolve, reject) => {
      const req = sender(options, (res) => {
        const status = res.statusCode ?? 502
        const headers = responseHeaders(res.headers)
        if (status === 204 || status === 205 || status === 304) {
          res.resume()
          resolve(new Response(null, { status, headers }))
          return
        }
        resolve(
          new Response(Readable.toWeb(res) as ReadableStream<Uint8Array>, {
            status,
            headers,
          }),
        )
      })
      req.on('error', reject)
      req.setTimeout(30_000, () => req.destroy(new Error('upstream timeout')))
      if (request.signal) request.signal.addEventListener('abort', () => req.destroy(), { once: true })
      if (request.body) {
        Readable.fromWeb(request.body as Parameters<typeof Readable.fromWeb>[0])
          .on('error', () => req.destroy())
          .pipe(req)
      } else {
        req.end()
      }
    })
    return response
  } catch (err) {
    console.error('[platform-v1-proxy] failed:', (err as Error).message)
    return Response.json(
      { error: { code: 'proxy_unavailable', message: 'upstream service is unavailable' } },
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
