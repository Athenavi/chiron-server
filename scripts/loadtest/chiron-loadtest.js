// chiron-server 多租户压测（k6）。
//
// 覆盖：会话列表/创建、消息提交（真跑 agent，需显式开启）、SSE 连接与断线重连、
// 文件上传。所有写操作默认关闭：先只读跑通，再逐步开启。
//
// 用法见同目录 README.md。最小示例：
//   LT_BASE_URL=https://acme.apps.example.com LT_TOKEN=... k6 run scripts/loadtest/chiron-loadtest.js
//
// 注意：不要用本脚本对生产租户做写压测。删改/管理类接口刻意不在这里出现。

import http from 'k6/http';
import { check, sleep } from 'k6';
import exec from 'k6/execution';
import { Counter, Rate, Trend } from 'k6/metrics';

const BASE = (__ENV.LT_BASE_URL || '').replace(/\/+$/, '');
const TOKEN = __ENV.LT_TOKEN || '';
const IDENTIFIER = __ENV.LT_IDENTIFIER || '';
const PASSWORD = __ENV.LT_PASSWORD || '';
const MODEL = __ENV.LT_MODEL || '';
const SESSION_ID = __ENV.LT_SESSION_ID || '';

const ALLOW_WRITES = isYes(__ENV.LT_ALLOW_WRITES);
const ALLOW_RUNS = isYes(__ENV.LT_ALLOW_RUNS);
const INTERRUPT = (__ENV.LT_INTERRUPT || 'yes').toLowerCase() !== 'no';
const ENABLE_SSE = (__ENV.LT_ENABLE_SSE || 'yes').toLowerCase() !== 'no';
const ENABLE_UPLOAD = (__ENV.LT_ENABLE_UPLOAD || 'yes').toLowerCase() !== 'no';

const SSE_TIMEOUT = __ENV.LT_SSE_TIMEOUT || '8s';
const SSE_TIMEOUT_MS = durationToMs(SSE_TIMEOUT);
const LAST_EVENT_ID = __ENV.LT_LAST_EVENT_ID || '0';

const SCENARIO = (__ENV.LT_SCENARIO || 'smoke').toLowerCase();
const VUS = toInt(__ENV.LT_VUS, 5);
const RAMP = __ENV.LT_RAMP || '30s';
const HOLD = __ENV.LT_HOLD || '2m';
const DURATION = __ENV.LT_DURATION || '10m';
const HOST_MAP = __ENV.LT_HOST_MAP || '';

// ---- 安全闸门 -------------------------------------------------------------

if (!BASE) {
  throw new Error('LT_BASE_URL 未设置（示例：https://acme.apps.example.com）');
}
if (!/^https?:\/\/[^/]+/.test(BASE)) {
  throw new Error('LT_BASE_URL 必须是 http(s)://host[:port] 形式');
}
if (!TOKEN && !(IDENTIFIER && PASSWORD)) {
  throw new Error('需要 LT_TOKEN，或 LT_IDENTIFIER + LT_PASSWORD（setup 里只登录一次）');
}
// 生产防护：命中常见生产命名且未显式放行时直接拒绝启动。
const BASE_HOST = BASE.replace(/^https?:\/\//, '').split('/')[0];
if (/(^|\.)(prod|production|live)([.:]|$)/i.test(BASE_HOST) && !isYes(__ENV.LT_ALLOW_PROD)) {
  throw new Error('目标看起来是生产环境；确认后用 LT_ALLOW_PROD=yes 显式放行');
}
if (ALLOW_RUNS && !ALLOW_WRITES) {
  throw new Error('LT_ALLOW_RUNS=yes 需要同时设置 LT_ALLOW_WRITES=yes');
}
if (!ALLOW_WRITES && !SESSION_ID) {
  // 只读模式下没有会话就没法测 SSE；照常运行，但报告里会看到 sse 检查被跳过。
  console.warn('未开启写操作且未提供 LT_SESSION_ID：SSE 场景将被跳过');
}
if (!ALLOW_WRITES && ENABLE_UPLOAD) {
  console.warn('未开启写操作：上传场景将被跳过');
}

// ---- 指标 ----------------------------------------------------------------

const sessionListDuration = new Trend('session_list_duration');
const sessionCreateDuration = new Trend('session_create_duration');
const messageSubmitDuration = new Trend('message_submit_duration');
const uploadDuration = new Trend('upload_duration');
const sseStreamAlive = new Rate('sse_stream_alive');
const sseProbeAttempts = new Counter('sse_probe_attempts');
const skippedSteps = new Counter('skipped_steps');

// ---- 选项 ----------------------------------------------------------------

const thresholds = {
  checks: ['rate>0.99'],
  'http_req_duration{expected_response:true}': ['p(95)<800', 'p(99)<2500'],
  'http_req_failed{expected_response:true}': ['rate<0.01'],
  session_list_duration: ['p(95)<500', 'p(99)<1500'],
};
if (ALLOW_WRITES) {
  thresholds.session_create_duration = ['p(95)<1200', 'p(99)<3000'];
  if (ENABLE_UPLOAD) {
    thresholds.upload_duration = ['p(95)<3000'];
  }
}
if (ALLOW_RUNS) {
  thresholds.message_submit_duration = ['p(95)<2000', 'p(99)<5000'];
}
if (ENABLE_SSE && (ALLOW_WRITES || SESSION_ID)) {
  thresholds.sse_stream_alive = ['rate>0.98'];
}

export const options = {
  scenarios: buildScenarios(),
  thresholds: thresholds,
  hosts: parseHostMap(HOST_MAP),
  // SSE 连接是长连接，收尾给足时间，避免被强杀导致 "interrupted iterations"。
  gracefulStop: '15s',
  summaryTrendStats: ['avg', 'min', 'med', 'p(90)', 'p(95)', 'p(99)', 'max'],
};

export function setup() {
  if (TOKEN) {
    return { token: TOKEN, source: 'env' };
  }
  const res = http.post(
    `${BASE}/api/v1/auth/login`,
    JSON.stringify({ identifier: IDENTIFIER, password: PASSWORD }),
    { headers: { 'Content-Type': 'application/json' }, tags: { step: 'auth.login' } },
  );
  check(res, { 'login 200': (r) => r.status === 200 });
  if (res.status !== 200) {
    throw new Error(`登录失败：HTTP ${res.status}（不会重试，避免触发登录锁定）`);
  }
  const body = safeJson(res.body);
  const token = body && body.data && body.data.token;
  if (!token) {
    throw new Error('登录响应里没有 data.token');
  }
  return { token: token, source: 'login' };
}

export default function (data) {
  const vu = exec.vu.idInTest;
  const iter = exec.scenario.iterationInTest;
  const headers = authHeaders(data.token);
  const stamp = `${vu}-${iter}-${Date.now()}`;

  // 1) 读：会话列表（登录后最常见的首屏请求）。
  const list = http.get(`${BASE}/api/v1/sessions?limit=20`, {
    headers: headers,
    tags: { step: 'session.list' },
  });
  sessionListDuration.add(list.timings.duration);
  check(list, { 'GET /sessions -> 200': (r) => r.status === 200 });
  sleep(rand(0.2, 0.8));

  let sessionId = SESSION_ID;

  // 2) 写：创建会话（默认关闭）。
  if (ALLOW_WRITES) {
    const created = http.post(
      `${BASE}/api/v1/sessions`,
      JSON.stringify({ title: `loadtest ${stamp}` }),
      { headers: headers, tags: { step: 'session.create' } },
    );
    sessionCreateDuration.add(created.timings.duration);
    const createdOk = check(created, { 'POST /sessions -> 200': (r) => r.status === 200 });
    if (createdOk) {
      const body = safeJson(created.body);
      if (body && body.data && body.data.session && body.data.session.id) {
        sessionId = body.data.session.id;
      }
    }
  } else {
    skippedSteps.add(1, { step: 'session.create' });
  }

  // 3) 写 + 模型成本：提交一轮消息（默认关闭；默认提交后立即中断以限制消耗）。
  if (ALLOW_RUNS && sessionId) {
    const payload = { content: `loadtest ping ${stamp}`, client_id: `lt-${stamp}` };
    if (MODEL) {
      payload.model = MODEL;
    }
    const submitted = http.post(
      `${BASE}/api/v1/sessions/${sessionId}/messages`,
      JSON.stringify(payload),
      {
        headers: withHeaders(headers, { 'Idempotency-Key': `lt-${stamp}` }),
        tags: { step: 'message.submit' },
      },
    );
    messageSubmitDuration.add(submitted.timings.duration);
    const accepted = check(submitted, {
      'POST messages -> 200/409': (r) => r.status === 200 || r.status === 409,
    });
    if (accepted && INTERRUPT && submitted.status === 200) {
      // 立刻中断，避免压测把 token 成本与沙箱时间打满。不属于破坏性操作。
      http.post(
        `${BASE}/api/v1/sessions/${sessionId}/interrupt`,
        JSON.stringify({ reason: 'loadtest' }),
        { headers: headers, tags: { step: 'session.interrupt' } },
      );
    }
  } else if (ALLOW_WRITES) {
    skippedSteps.add(1, { step: 'message.submit' });
  }

  // 4) SSE：连接一次 + 用 Last-Event-ID 重连一次。
  //    stock k6 不能做到边读边解析 SSE，这里用"短超时探针"：请求在超时前一直未结束
  //    说明反代没有缓冲/提前关闭、连接保持打开；返回 200 则说明流被过早结束。
  if (ENABLE_SSE && sessionId) {
    probeStream(sessionId, headers, null);
    sleep(rand(0.1, 0.4));
    probeStream(sessionId, headers, LAST_EVENT_ID);
  } else if (ENABLE_SSE) {
    skippedSteps.add(1, { step: 'sse.probe' });
  }

  // 5) 写：上传一个 1KB 文本文件（默认关闭）。
  if (ALLOW_WRITES && ENABLE_UPLOAD) {
    const blob = `chiron loadtest payload ${stamp} ${'x'.repeat(1024)}`;
    const uploaded = http.post(
      `${BASE}/api/v1/media?filename=loadtest-${stamp}.txt`,
      blob,
      {
        headers: withHeaders(headers, { 'Content-Type': 'text/plain' }),
        tags: { step: 'media.upload' },
      },
    );
    uploadDuration.add(uploaded.timings.duration);
    check(uploaded, { 'POST /media -> 201': (r) => r.status === 201 });
  } else if (ENABLE_UPLOAD) {
    skippedSteps.add(1, { step: 'media.upload' });
  }

  sleep(rand(0.5, 1.5));
}

// ---- 步骤实现 -------------------------------------------------------------

function probeStream(sessionId, headers, lastEventId) {
  const started = Date.now();
  const requestHeaders = { Accept: 'text/event-stream', 'Cache-Control': 'no-cache' };
  if (lastEventId !== null && lastEventId !== undefined && lastEventId !== '') {
    requestHeaders['Last-Event-ID'] = String(lastEventId);
  }
  const res = http.get(`${BASE}/api/v1/sessions/${sessionId}/events`, {
    headers: withHeaders(headers, requestHeaders),
    timeout: SSE_TIMEOUT,
    tags: { step: 'sse.probe', reconnect: lastEventId ? 'yes' : 'no' },
  });
  const elapsed = Date.now() - started;

  // 判定：请求耗掉了整个超时窗口、状态 0、错误是超时 → 流保持打开 = 通过。
  const errText = (res.error || '').toLowerCase();
  const timedOut =
    res.status === 0 &&
    (errText.indexOf('timeout') >= 0 ||
      errText.indexOf('deadline') >= 0 ||
      res.error_code === 1050) &&
    elapsed >= SSE_TIMEOUT_MS * 0.9;

  sseProbeAttempts.add(1, { reconnect: lastEventId ? 'yes' : 'no' });
  sseStreamAlive.add(timedOut);

  check(res, {
    'SSE 保持打开（未缓冲/未提前关闭）': () => timedOut,
    'SSE 未返回 401/404/502': (r) => r.status === 0 || r.status === 200,
  });
}

function authHeaders(token) {
  return { Authorization: `Bearer ${token}` };
}

function withHeaders(base, extra) {
  const out = {};
  Object.keys(base).forEach((k) => {
    out[k] = base[k];
  });
  Object.keys(extra).forEach((k) => {
    out[k] = extra[k];
  });
  return out;
}

// ---- 配置 -----------------------------------------------------------------

function buildScenarios() {
  if (SCENARIO === 'smoke') {
    return {
      smoke: {
        executor: 'shared-iterations',
        vus: Math.max(1, Math.min(VUS, 5)),
        iterations: Math.max(1, VUS),
        exec: 'default',
        tags: { scenario: 'smoke' },
      },
    };
  }
  if (SCENARIO === 'soak') {
    return {
      soak: {
        executor: 'constant-vus',
        vus: VUS,
        duration: DURATION,
        exec: 'default',
        tags: { scenario: 'soak' },
      },
    };
  }
  return {
    ramp: {
      executor: 'ramping-vus',
      startVUs: 0,
      stages: [
        { duration: RAMP, target: VUS },
        { duration: HOLD, target: VUS },
        { duration: RAMP, target: 0 },
      ],
      exec: 'default',
      tags: { scenario: 'ramp' },
    },
  };
}

// LT_HOST_MAP="acme.apps.example.com=127.0.0.1:8443,platform.example.com=127.0.0.1:8443"
function parseHostMap(raw) {
  const map = {};
  if (!raw) {
    return map;
  }
  raw.split(',').forEach((pair) => {
    const parts = pair.split('=');
    if (parts.length === 2 && parts[0].trim() && parts[1].trim()) {
      map[parts[0].trim()] = parts[1].trim();
    }
  });
  return map;
}

// ---- 小工具 ---------------------------------------------------------------

function isYes(v) {
  const s = (v || '').toLowerCase();
  return s === '1' || s === 'yes' || s === 'true' || s === 'on';
}

function toInt(v, fallback) {
  const n = parseInt(v, 10);
  return isNaN(n) || n <= 0 ? fallback : n;
}

function rand(min, max) {
  return Math.random() * (max - min) + min;
}

function durationToMs(raw) {
  const m = /^(\d+)(ms|s|m)$/.exec(String(raw).trim());
  if (!m) {
    return 8000;
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
