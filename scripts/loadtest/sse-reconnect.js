// SSE 真实流式压测（k6 + k6/x/sse）。
//
// 与 chiron-loadtest.js 的"超时探针"不同，这里真正解析事件流：统计首事件延迟、
// 心跳间隔、事件数，并用 Last-Event-ID 重连验证回放/续传。
//
// 运行：
//   LT_BASE_URL=https://acme.apps.example.com LT_TOKEN=... LT_SESSION_ID=<uuid> \
//     k6 run scripts/loadtest/sse-reconnect.js
//
// 注意：k6 新版本会自动解析 k6/x/sse 扩展；旧版本需要用 xk6 自行构建二进制
//   xk6 build --with github.com/phymbert/xk6-sse@latest
//
// 会话空闲时服务端只有 15s 一次 heartbeat，因此首事件延迟默认不能当 SLA 用；
// 只有在跑 run 时（LT_SSE_EXPECT_ACTIVE_RUN=yes）才有意义。

import sse from 'k6/x/sse';
import http from 'k6/http';
import { check } from 'k6';
import { Counter, Rate, Trend } from 'k6/metrics';

const BASE = (__ENV.LT_BASE_URL || '').replace(/\/+$/, '');
const TOKEN = __ENV.LT_TOKEN || '';
const IDENTIFIER = __ENV.LT_IDENTIFIER || '';
const PASSWORD = __ENV.LT_PASSWORD || '';
const SESSION_ID = __ENV.LT_SESSION_ID || '';
const HOLD = __ENV.LT_SSE_HOLD || '20s';
const HOLD_MS = durationToMs(HOLD);
const MIN_EVENTS = toInt(__ENV.LT_SSE_EVENTS, 3);
const RECONNECTS = toInt(__ENV.LT_SSE_RECONNECTS, 2);
const LAST_EVENT_ID = __ENV.LT_LAST_EVENT_ID || '';
const EXPECT_ACTIVE_RUN = (__ENV.LT_SSE_EXPECT_ACTIVE_RUN || 'no').toLowerCase() === 'yes';

const VUS = toInt(__ENV.LT_VUS, 2);
const DURATION = __ENV.LT_DURATION || '2m';
const HOST_MAP = __ENV.LT_HOST_MAP || '';

if (!BASE) {
  throw new Error('LT_BASE_URL 未设置');
}
if (!SESSION_ID) {
  throw new Error('LT_SESSION_ID 未设置（必须是一个已存在、属于该 token 的会话）');
}
if (!TOKEN && !(IDENTIFIER && PASSWORD)) {
  throw new Error('需要 LT_TOKEN，或 LT_IDENTIFIER + LT_PASSWORD');
}
const BASE_HOST = BASE.replace(/^https?:\/\//, '').split('/')[0];
if (/(^|\.)(prod|production|live)([.:]|$)/i.test(BASE_HOST) && !isYes(__ENV.LT_ALLOW_PROD)) {
  throw new Error('目标看起来是生产环境；确认后用 LT_ALLOW_PROD=yes 显式放行');
}

const firstEventMs = new Trend('sse_first_event_ms');
const eventsTotal = new Counter('sse_events_total');
const heartbeats = new Counter('sse_heartbeats_total');
const gaps = new Counter('sse_gap_events');
const streamFailures = new Rate('sse_stream_failures');
const reconnectFailures = new Rate('sse_reconnect_failures');

const thresholds = {
  checks: ['rate>0.99'],
  sse_stream_failures: ['rate<0.01'],
  sse_reconnect_failures: ['rate<0.01'],
  sse_events_total: ['count>0'],
};
if (EXPECT_ACTIVE_RUN) {
  // 有活跃 run 时，首事件应明显快于心跳周期（15s）。
  thresholds.sse_first_event_ms = ['p(95)<3000'];
}

export const options = {
  scenarios: {
    sse: {
      executor: 'constant-vus',
      vus: VUS,
      duration: DURATION,
      gracefulStop: '20s',
    },
  },
  thresholds: thresholds,
  hosts: parseHostMap(HOST_MAP),
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  if (TOKEN) {
    return { token: TOKEN };
  }
  const res = http.post(
    `${BASE}/api/v1/auth/login`,
    JSON.stringify({ identifier: IDENTIFIER, password: PASSWORD }),
    { headers: { 'Content-Type': 'application/json' }, tags: { step: 'auth.login' } },
  );
  if (res.status !== 200) {
    throw new Error(`登录失败：HTTP ${res.status}（不会重试，避免登录锁定）`);
  }
  const body = safeJson(res.body);
  const token = body && body.data && body.data.token;
  if (!token) {
    throw new Error('登录响应里没有 data.token');
  }
  return { token: token };
}

export default function (data) {
  let cursor = LAST_EVENT_ID;

  for (let attempt = 0; attempt < RECONNECTS; attempt++) {
    const result = openOnce(data.token, cursor, attempt === 0 ? 'connect' : 'reconnect');
    check(result, {
      'SSE 流打开': (r) => r.opened,
      'SSE 收到事件': (r) => r.events > 0,
      'SSE 无错误': (r) => r.error === '',
    });
    if (attempt === 0) {
      streamFailures.add(!result.opened || result.events === 0 || result.error !== '');
    } else {
      reconnectFailures.add(!result.opened || result.error !== '');
    }
    if (result.lastId) {
      cursor = result.lastId;
    }
    // 每轮建立新连接，模拟前端断线后带 Last-Event-ID 重连。
  }
}

function openOnce(token, lastEventId, phase) {
  const url = `${BASE}/api/v1/sessions/${SESSION_ID}/events`;
  const headers = { Authorization: `Bearer ${token}`, Accept: 'text/event-stream' };
  if (lastEventId) {
    headers['Last-Event-ID'] = String(lastEventId);
  }
  const params = {
    method: 'GET',
    headers: headers,
    timeout: HOLD,
    tags: { phase: phase },
  };

  const started = Date.now();
  let opened = false;
  let events = 0;
  let error = '';
  let lastId = '';
  let firstAt = 0;

  const res = sse.open(url, params, function (client) {
    client.on('open', function () {
      opened = true;
    });
    client.on('event', function (event) {
      events += 1;
      eventsTotal.add(1, { phase: phase });
      if (!firstAt) {
        firstAt = Date.now();
        firstEventMs.add(firstAt - started, { phase: phase });
      }
      if (event.id) {
        lastId = String(event.id);
      }
      const envelope = safeJson(event.data);
      const type = envelope && envelope.type ? envelope.type : '';
      if (type === 'heartbeat') {
        heartbeats.add(1);
      }
      if (type === 'run.gap') {
        // 超出保留窗口，客户端应重拉 messages/runs；压测里只计数。
        gaps.add(1);
      }
      if (events >= MIN_EVENTS || Date.now() - started >= HOLD_MS) {
        client.close();
      }
    });
    client.on('error', function (e) {
      error = e && e.error ? String(e.error()) : 'unknown';
    });
  });

  if (res && res.status === 200) {
    opened = true;
  }
  return { opened: opened, events: events, error: error, lastId: lastId };
}

// ---- 小工具 ---------------------------------------------------------------

function parseHostMap(raw) {
  const map = {};
  if (!raw) {
    return map;
  }
  raw.split(',').forEach(function (pair) {
    const parts = pair.split('=');
    if (parts.length === 2 && parts[0].trim() && parts[1].trim()) {
      map[parts[0].trim()] = parts[1].trim();
    }
  });
  return map;
}

function toInt(v, fallback) {
  const n = parseInt(v, 10);
  return isNaN(n) || n <= 0 ? fallback : n;
}

function isYes(v) {
  const s = (v || '').toLowerCase();
  return s === '1' || s === 'yes' || s === 'true' || s === 'on';
}

function durationToMs(raw) {
  const m = /^(\d+)(ms|s|m)$/.exec(String(raw).trim());
  if (!m) {
    return 20000;
  }
  const n = parseInt(m[1], 10);
  if (m[2] === 'ms') return n;
  if (m[2] === 's') return n * 1000;
  return n * 60000;
}

function safeJson(text) {
  try {
    return JSON.parse(text);
  } catch (e) {
    return null;
  }
}
