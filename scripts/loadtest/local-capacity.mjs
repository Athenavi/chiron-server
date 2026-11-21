#!/usr/bin/env node
// 本地容量冒烟：并发创建会话、提交 run、轮询终态，并压 SSE 连接。
// 需要 local server + mock provider；不依赖真实模型或 Docker 沙箱。
// 用法：
//   BASE=http://127.0.0.1:18084 HOST=bench.apps.local TOKEN=... \
//   SESSIONS=30 CONCURRENCY=10 SSE=20 node scripts/loadtest/local-capacity.mjs
import http from 'node:http'

const BASE = new URL(process.env.BASE || '')
const HOST = process.env.HOST || ''
const TOKEN = process.env.TOKEN || ''
const SESSIONS = Number(process.env.SESSIONS || 30)
const CONCURRENCY = Number(process.env.CONCURRENCY || 10)
const SSE = Number(process.env.SSE || 20)
const POLL_MS = Number(process.env.POLL_MS || 100)
const TIMEOUT_MS = Number(process.env.TIMEOUT_MS || 120000)

if (!HOST || !TOKEN) {
  console.error('BASE, HOST and TOKEN are required')
  process.exit(2)
}

function request(path, { method = 'GET', body, headers = {}, timeout = 30000 } = {}) {
  return new Promise((resolve, reject) => {
    const payload = body === undefined ? undefined : JSON.stringify(body)
    const req = http.request(
      {
        hostname: BASE.hostname,
        port: BASE.port || 80,
        path: `/api/v1${path}`,
        method,
        timeout,
        headers: {
          Host: HOST,
          Authorization: `Bearer ${TOKEN}`,
          Accept: 'application/json',
          ...(payload ? { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(payload) } : {}),
          ...headers,
        },
      },
      (res) => {
        let text = ''
        res.setEncoding('utf8')
        res.on('data', (chunk) => (text += chunk))
        res.on('end', () => {
          let data
          try {
            data = text ? JSON.parse(text) : {}
          } catch {
            data = { raw: text }
          }
          if ((res.statusCode || 500) >= 400) {
            reject(new Error(`${method} ${path} ${res.statusCode} ${data?.error?.code || ''}`))
            return
          }
          resolve(data.data ?? data)
        })
      },
    )
    req.on('timeout', () => req.destroy(new Error(`${method} ${path} timeout`)))
    req.on('error', reject)
    if (payload) req.write(payload)
    req.end()
  })
}

async function createSession(index) {
  const data = await request('/sessions', { method: 'POST', body: { title: `bench-${index}` } })
  return data.session.id
}

async function waitRun(runId, started) {
  while (Date.now() - started < TIMEOUT_MS) {
    const data = await request(`/runs/${runId}`)
    const status = data.run?.status
    if (status === 'completed' || status === 'failed' || status === 'cancelled') {
      return { status, ms: Date.now() - started }
    }
    await new Promise((resolve) => setTimeout(resolve, POLL_MS))
  }
  return { status: 'timeout', ms: Date.now() - started }
}

async function runOne(sessionId) {
  const started = Date.now()
  const submitted = await request(`/sessions/${sessionId}/messages`, {
    method: 'POST',
    body: { content: 'hello', client_id: `bench-${crypto.randomUUID()}` },
  })
  const result = await waitRun(submitted.run_id, started)
  return { sessionId, runId: submitted.run_id, ...result }
}

function openSse(sessionId) {
  return new Promise((resolve) => {
    let settled = false
    const done = (ok) => {
      if (settled) return
      settled = true
      resolve(ok)
    }
    const req = http.request(
      {
        hostname: BASE.hostname,
        port: BASE.port || 80,
        path: `/api/v1/sessions/${sessionId}/events`,
        method: 'GET',
        timeout: 5000,
        headers: {
          Host: HOST,
          Authorization: `Bearer ${TOKEN}`,
          Accept: 'text/event-stream',
        },
      },
      (res) => {
        if (res.statusCode !== 200) {
          res.resume()
          done(false)
          return
        }
        res.once('data', () => done(true))
        setTimeout(() => {
          req.destroy()
          done(true)
        }, 500)
      },
    )
    req.on('error', () => done(false))
    setTimeout(() => {
      req.destroy()
      done(false)
    }, 5000)
    req.end()
  })
}

function percentile(values, p) {
  if (values.length === 0) return 0
  const sorted = [...values].sort((a, b) => a - b)
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)]
}

async function main() {
  const t0 = Date.now()
  const sessionIds = []
  for (let i = 0; i < SESSIONS; i += 1) sessionIds.push(await createSession(i))
  const createMs = Date.now() - t0

  const results = []
  let cursor = 0
  const workers = Array.from({ length: Math.min(CONCURRENCY, sessionIds.length) }, async () => {
    while (cursor < sessionIds.length) {
      const index = cursor
      cursor += 1
      try {
        results.push(await runOne(sessionIds[index]))
      } catch (error) {
        results.push({ status: 'error', error: String(error), ms: 0 })
      }
    }
  })
  const runStart = Date.now()
  await Promise.all(workers)
  const runWallMs = Date.now() - runStart

  const sseStart = Date.now()
  const sseResults = await Promise.all(Array.from({ length: SSE }, () => openSse(sessionIds[0])))
  const sseMs = Date.now() - sseStart

  const durations = results.filter((r) => r.status === 'completed').map((r) => r.ms)
  const summary = {
    sessions: SESSIONS,
    concurrency: CONCURRENCY,
    create_ms: createMs,
    run_wall_ms: runWallMs,
    runs_completed: durations.length,
    runs_failed: results.filter((r) => r.status !== 'completed').length,
    run_p50_ms: percentile(durations, 50),
    run_p95_ms: percentile(durations, 95),
    run_max_ms: Math.max(0, ...durations),
    sse_requested: SSE,
    sse_ok: sseResults.filter(Boolean).length,
    sse_wall_ms: sseMs,
  }
  console.log(JSON.stringify(summary, null, 2))
  if (summary.runs_failed > 0) process.exitCode = 1
}

main().catch((error) => {
  console.error(error)
  process.exit(1)
})
