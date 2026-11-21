#!/usr/bin/env node
// 本地 OpenAI 兼容 mock（仅开发/容量冒烟）：让 run 在没有真实模型和沙箱时也能完成。
// 用法：MOCK_PORT=19090 node scripts/loadtest/mock-provider.mjs
import http from 'node:http'

const port = Number(process.env.MOCK_PORT || 19090)

const server = http.createServer((req, res) => {
  if (req.method === 'GET' && (req.url === '/v1/models' || req.url === '/models')) {
    res.writeHead(200, { 'content-type': 'application/json' })
    res.end(JSON.stringify({ data: [{ id: 'mock', object: 'model' }] }))
    return
  }
  if (req.method !== 'POST' || !req.url?.includes('/chat/completions')) {
    res.writeHead(404).end()
    return
  }

  let body = ''
  req.on('data', (chunk) => (body += chunk))
  req.on('end', () => {
    let stream = true
    try {
      stream = JSON.parse(body).stream !== false
    } catch {
      /* 默认流式 */
    }
    if (!stream) {
      res.writeHead(200, { 'content-type': 'application/json' })
      res.end(
        JSON.stringify({
          id: 'chatcmpl-mock',
          object: 'chat.completion',
          choices: [{ index: 0, message: { role: 'assistant', content: 'ok' }, finish_reason: 'stop' }],
          usage: { prompt_tokens: 4, completion_tokens: 1, total_tokens: 5 },
        }),
      )
      return
    }

    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
      connection: 'keep-alive',
    })
    const chunk = (delta, finish = null) =>
      `data: ${JSON.stringify({
        id: 'chatcmpl-mock',
        object: 'chat.completion.chunk',
        choices: [{ index: 0, delta, finish_reason: finish }],
      })}\n\n`

    res.write(chunk({ role: 'assistant', content: '' }))
    setTimeout(() => {
      res.write(chunk({ content: 'ok' }))
      res.write(chunk({}, 'stop'))
      res.write(
        `data: ${JSON.stringify({
          id: 'chatcmpl-mock',
          object: 'chat.completion.chunk',
          choices: [],
          usage: { prompt_tokens: 4, completion_tokens: 1, total_tokens: 5 },
        })}\n\n`,
      )
      res.write('data: [DONE]\n\n')
      res.end()
    }, Number(process.env.MOCK_DELAY_MS || 20))
  })
})

server.listen(port, '127.0.0.1', () => {
  console.log(`mock provider listening on 127.0.0.1:${port}`)
})
