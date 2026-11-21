/**
 * API 客户端：页面只依赖这个文件（以及 `lib/types.ts` / `lib/sse.ts`）。
 *
 * 传输层可切换：
 * - `NEXT_PUBLIC_USE_MOCK=1`（默认）→ `lib/mock.ts` 的内存实现；
 * - 否则走真实 HTTP，`NEXT_PUBLIC_API_BASE` 留空 = 同源 `/api/v1`
 *   （开发时由 `app/api/v1/[...path]/route.ts` 反代到 Rust 服务）。
 *
 * 所有请求都带 `credentials: 'include'`（httpOnly `chiron_token` cookie）；
 * 可选 Bearer：token 存在 sessionStorage，只作为本地开发/无 cookie 环境的兜底。
 */

import * as mock from './mock'
import { uiText } from './ui-locale'
import {
  ApiError,
  type AddCatalogModelRequest,
  type AddModelRouteRequest,
  type AddProviderKeyRequest,
  type AdminModelProvider,
  type AdminModelsResult,
  type ApprovalDecision,
  type AuthPayload,
  type CatalogModel,
  type ChangePasswordResult,
  type ChatSession,
  type ClearMemoriesResult,
  type CreateMemoryRequest,
  type CreatePluginRequest,
  type CreateSessionRequest,
  type CreateSessionResult,
  type CreateSkillRequest,
  type CreateUserRequest,
  type CreateUserResult,
  type DiscoverModelsRequest,
  type DiscoverModelsResult,
  type ListMemoriesParams,
  type ListMessagesParams,
  type ListMessagesResult,
  type ListSessionsParams,
  type ListSessionsResult,
  type ListUsersParams,
  type ListUsersResult,
  type ListAuditResult,
  type AuditParams,
  type UsageParams,
  type UsageReport,
  type ListMediaParams,
  type ListMediaResult,
  type MediaAsset,
  type LoginRequest,
  type LoginResult,
  type MarketItem,
  type McpProbeResult,
  type McpServer,
  type McpServerInput,
  type McpTool,
  type Memory,
  type MessageInputContent,
  type ModelCatalog,
  type ModelProviderKey,
  type ModelRoute,
  type Plugin,
  type ProviderProbeResult,
  type ResetUserPasswordResult,
  type RunDetailResult,
  type SendMessageOptions,
  type SendMessageResult,
  type SessionCheckpointsResult,
  type SessionDetailResult,
  type SessionFilesResult,
  type SessionResult,
  type SessionSubagentsResult,
  type Skill,
  type TenantModelsResult,
  type UninstallMarketItemResult,
  type UpdateCatalogModelRequest,
  type UpdateMemoryRequest,
  type UpdateModelRouteRequest,
  type UpdatePluginRequest,
  type UpdateProfileRequest,
  type UpdateProviderKeyRequest,
  type UpdateSessionRequest,
  type UpdateSessionResult,
  type UpdateSkillRequest,
  type HiddenModelsResult,
  type User,
} from './types'

// ── 模式与地址 ──────────────────────────────────────────────────────────

function readMockFlag(): boolean {
  const raw = process.env.NEXT_PUBLIC_USE_MOCK
  // 生产默认连真实 API；mock 必须显式打开，避免构建漏配时静默展示假数据。
  if (raw === undefined || raw === '') return process.env.NODE_ENV !== 'production'
  return raw === '1' || raw.toLowerCase() === 'true'
}

/** 是否使用内置 mock（构建期常量，服务端/客户端一致）。 */
export const USE_MOCK = readMockFlag()

export function isMockMode(): boolean {
  return USE_MOCK
}

/** 真实模式的 API 前缀；空字符串表示同源。 */
export function apiBase(): string {
  return (process.env.NEXT_PUBLIC_API_BASE || '').replace(/\/+$/, '')
}

const TOKEN_KEY = 'chiron_token'
// 生产请求靠 httpOnly cookie；sessionStorage 里的 Bearer 只在开发/无 cookie 环境使用。
const PERSIST_TOKEN = process.env.NODE_ENV !== 'production'

export function getToken(): string {
  if (!PERSIST_TOKEN || typeof window === 'undefined') return ''
  try {
    return window.sessionStorage.getItem(TOKEN_KEY) || ''
  } catch {
    return ''
  }
}

export function setToken(token: string | null | undefined): void {
  if (!PERSIST_TOKEN || typeof window === 'undefined') return
  try {
    if (token) window.sessionStorage.setItem(TOKEN_KEY, token)
    else window.sessionStorage.removeItem(TOKEN_KEY)
  } catch {
    // 隐私模式下 sessionStorage 可能不可用：忽略，cookie 仍然生效
  }
}

// ── 传输层 ──────────────────────────────────────────────────────────────

interface RequestOptions {
  method?: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE'
  body?: unknown
  headers?: Record<string, string>
  signal?: AbortSignal
}

function buildUrl(path: string, query?: Record<string, string | number | null | undefined>): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value === undefined || value === null || value === '') continue
    search.set(key, String(value))
  }
  const qs = search.toString()
  return `${apiBase()}/api/v1${path}${qs ? `?${qs}` : ''}`
}

async function readError(response: Response): Promise<ApiError> {
  let code = `http_${response.status}`
  let message =
    response.statusText || uiText('请求失败，请稍后重试', 'Request failed. Please try again.')
  try {
    const text = await response.text()
    if (text) {
      const body = JSON.parse(text) as { error?: { code?: string; message?: string } }
      if (body?.error) {
        code = body.error.code || code
        message = body.error.message || message
      }
    }
  } catch {
    // 非 JSON 响应：保留状态码信息
  }
  if (response.status === 401) {
    message = message || uiText('登录已过期，请重新登录', 'Your session has expired. Sign in again.')
  }
  return new ApiError(response.status, code, message)
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = 'GET', body, headers = {}, signal } = options
  const finalHeaders: Record<string, string> = { Accept: 'application/json', ...headers }
  if (body !== undefined) finalHeaders['Content-Type'] = 'application/json'
  const token = getToken()
  if (token) finalHeaders.Authorization = `Bearer ${token}`

  let response: Response
  try {
    response = await fetch(buildUrl(path), {
      method,
      credentials: 'include',
      cache: 'no-store',
      headers: finalHeaders,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
    })
  } catch (error) {
    throw ApiError.from(error, '网络连接失败，请检查服务是否可用')
  }

  if (!response.ok) throw await readError(response)
  if (response.status === 204) return undefined as T

  const text = await response.text()
  if (!text) return undefined as T
  let parsed: unknown
  try {
    parsed = JSON.parse(text)
  } catch {
    throw new ApiError(response.status, 'invalid_response', '服务返回了非 JSON 响应')
  }
  if (parsed && typeof parsed === 'object' && 'data' in parsed) {
    return (parsed as { data: T }).data
  }
  return parsed as T
}

function makeIdempotencyKey(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID()
  }
  return `idem_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 10)}`
}

// ── 认证 ────────────────────────────────────────────────────────────────

async function login(body: LoginRequest): Promise<LoginResult> {
  const result = USE_MOCK
    ? await mock.login(body)
    : await request<AuthPayload>('/auth/login', { method: 'POST', body })
  setToken(result.token)
  return result
}

async function logout(): Promise<void> {
  try {
    if (USE_MOCK) await mock.logout()
    else await request<Record<string, unknown>>('/auth/logout', { method: 'POST' })
  } catch (error) {
    // cookie 已失效时后端返回 401：本地状态仍需清理，不向上抛
    if (!(error instanceof ApiError) || error.status !== 401) throw error
  } finally {
    setToken(null)
  }
}

async function session(): Promise<SessionResult> {
  const result = USE_MOCK
    ? await mock.session()
    : await request<AuthPayload>('/auth/session', { method: 'GET' })
  setToken(result.token)
  return result
}

async function changePassword(oldPassword: string, newPassword: string): Promise<ChangePasswordResult> {
  if (USE_MOCK) return mock.changePassword(oldPassword, newPassword)
  const result = await request<ChangePasswordResult>('/auth/password', {
    method: 'POST',
    body: { old_password: oldPassword, new_password: newPassword },
  })
  setToken(result.token)
  return result
}

async function profile(): Promise<User> {
  if (USE_MOCK) return mock.profile()
  const data = await request<{ user: User }>('/auth/profile', { method: 'GET' })
  return data.user
}

async function updateProfile(body: UpdateProfileRequest): Promise<User> {
  if (USE_MOCK) return mock.updateProfile(body)
  const data = await request<{ user: User }>('/auth/profile', { method: 'PATCH', body })
  return data.user
}

async function listModels(): Promise<ModelCatalog> {
  if (USE_MOCK) return mock.listModels()
  return request<ModelCatalog>('/models', { method: 'GET' })
}

// ── 租户模型服务（管理端 / 用户目录 / 隐藏） ───────────────────────────

/**
 * 部分写接口可能返回行本身，也可能包成 `{ key: row }`；
 * 只有值是对象时才解包，避免把 `{ provider: 'deepseek' }` 里的字符串误当行。
 */
function unwrapRow<T>(data: unknown, key: string): T {
  if (data && typeof data === 'object') {
    const value = (data as Record<string, unknown>)[key]
    if (value && typeof value === 'object' && !Array.isArray(value)) return value as T
  }
  return data as T
}

async function adminModels(): Promise<AdminModelsResult> {
  if (USE_MOCK) return mock.adminModels()
  return request<AdminModelsResult>('/admin/models', { method: 'GET' })
}

async function addProviderKey(body: AddProviderKeyRequest): Promise<ModelProviderKey> {
  if (USE_MOCK) return mock.addProviderKey(body)
  const data = await request<unknown>('/admin/models/keys', { method: 'POST', body })
  return unwrapRow<ModelProviderKey>(data, 'key')
}

async function updateProviderKey(
  id: string,
  body: UpdateProviderKeyRequest,
): Promise<ModelProviderKey> {
  if (USE_MOCK) return mock.updateProviderKey(id, body)
  const data = await request<unknown>(`/admin/models/keys/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return unwrapRow<ModelProviderKey>(data, 'key')
}

async function deleteProviderKey(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteProviderKey(id)
  await request<Record<string, unknown>>(`/admin/models/keys/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

async function setProviderEnabled(
  provider: string,
  enabled: boolean,
): Promise<AdminModelProvider> {
  if (USE_MOCK) return mock.setProviderEnabled(provider, enabled)
  const data = await request<unknown>(
    `/admin/models/providers/${encodeURIComponent(provider)}`,
    { method: 'PATCH', body: { enabled } },
  )
  return unwrapRow<AdminModelProvider>(data, 'provider')
}

async function addCatalogModel(body: AddCatalogModelRequest): Promise<CatalogModel> {
  if (USE_MOCK) return mock.addCatalogModel(body)
  const data = await request<unknown>('/admin/models/catalog', { method: 'POST', body })
  return unwrapRow<CatalogModel>(data, 'model')
}

async function updateCatalogModel(
  model: string,
  body: UpdateCatalogModelRequest,
): Promise<CatalogModel> {
  if (USE_MOCK) return mock.updateCatalogModel(model, body)
  const data = await request<unknown>(`/admin/models/catalog/${encodeURIComponent(model)}`, {
    method: 'PATCH',
    body,
  })
  return unwrapRow<CatalogModel>(data, 'model')
}

async function deleteCatalogModel(model: string): Promise<void> {
  if (USE_MOCK) return mock.deleteCatalogModel(model)
  await request<Record<string, unknown>>(`/admin/models/catalog/${encodeURIComponent(model)}`, {
    method: 'DELETE',
  })
}

async function addModelRoute(body: AddModelRouteRequest): Promise<ModelRoute> {
  if (USE_MOCK) return mock.addModelRoute(body)
  const data = await request<unknown>('/admin/models/routes', { method: 'POST', body })
  return unwrapRow<ModelRoute>(data, 'route')
}

async function updateModelRoute(
  id: string,
  body: UpdateModelRouteRequest,
): Promise<ModelRoute> {
  if (USE_MOCK) return mock.updateModelRoute(id, body)
  const data = await request<unknown>(`/admin/models/routes/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return unwrapRow<ModelRoute>(data, 'route')
}

async function deleteModelRoute(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteModelRoute(id)
  await request<Record<string, unknown>>(`/admin/models/routes/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

/**
 * 自动发现：`persist=false` 只探测（写入前预览），`persist=true` 落库并生成路由。
 * 响应容错：缺 `results` 时按空数组处理，避免面板因契约漂移崩溃。
 */
async function discoverModels(body: DiscoverModelsRequest = {}): Promise<DiscoverModelsResult> {
  if (USE_MOCK) return mock.discoverModels(body)
  const data = await request<Partial<DiscoverModelsResult>>('/admin/models/discover', {
    method: 'POST',
    body,
  })
  return { results: Array.isArray(data?.results) ? data.results : [] }
}

/** 健康探测：只探测连通性与模型数，不落库。 */
async function probeProvider(provider: string): Promise<ProviderProbeResult> {
  if (USE_MOCK) return mock.probeProvider(provider)
  const data = await request<Partial<ProviderProbeResult>>(
    `/admin/models/providers/${encodeURIComponent(provider)}/probe`,
    { method: 'POST' },
  )
  return { ok: Boolean(data?.ok), models: data?.models, error: data?.error }
}

/** 普通用户可见目录（启用且带 provider 信息）。 */
async function listTenantModels(): Promise<TenantModelsResult> {
  if (USE_MOCK) return mock.listTenantModels()
  const data = await request<Partial<TenantModelsResult>>('/models', { method: 'GET' })
  return {
    models: Array.isArray(data?.models) ? data.models : [],
    providers: Array.isArray(data?.providers) ? data.providers : [],
  }
}

async function getHiddenModels(): Promise<string[]> {
  if (USE_MOCK) return (await mock.getHiddenModels()).models
  const data = await request<Partial<HiddenModelsResult>>('/models/hidden', { method: 'GET' })
  return Array.isArray(data?.models) ? data.models : []
}

async function setHiddenModels(models: string[]): Promise<string[]> {
  if (USE_MOCK) return (await mock.setHiddenModels(models)).models
  const data = await request<Partial<HiddenModelsResult>>('/models/hidden', {
    method: 'PUT',
    body: { models },
  })
  return Array.isArray(data?.models) ? data.models : models
}

// ── 会话 ────────────────────────────────────────────────────────────────

async function listSessions(params: ListSessionsParams = {}): Promise<ListSessionsResult> {
  if (USE_MOCK) return mock.listSessions(params)
  const search = new URLSearchParams()
  if (params.status) search.set('status', params.status)
  if (params.cursor) search.set('cursor', params.cursor)
  if (params.limit) search.set('limit', String(params.limit))
  const qs = search.toString()
  return request<ListSessionsResult>(`/sessions${qs ? `?${qs}` : ''}`, { method: 'GET' })
}

async function createSession(body: CreateSessionRequest = {}): Promise<CreateSessionResult> {
  if (USE_MOCK) return mock.createSession(body)
  return request<CreateSessionResult>('/sessions', { method: 'POST', body })
}

async function getSession(id: string): Promise<SessionDetailResult> {
  if (USE_MOCK) return mock.getSession(id)
  return request<SessionDetailResult>(`/sessions/${encodeURIComponent(id)}`, { method: 'GET' })
}

async function updateSession(id: string, body: UpdateSessionRequest): Promise<UpdateSessionResult> {
  if (USE_MOCK) return mock.updateSession(id, body)
  const payload: Record<string, unknown> = {}
  if (body.title !== undefined) payload.title = body.title
  if (body.model !== undefined) payload.model = body.model
  if (body.archived !== undefined) payload.archived = body.archived
  else if (body.status !== undefined) payload.archived = body.status === 'archived'
  return request<UpdateSessionResult>(`/sessions/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body: payload,
  })
}

async function deleteSession(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteSession(id)
  await request<Record<string, unknown>>(`/sessions/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

// ── 消息 ────────────────────────────────────────────────────────────────

async function listMessages(
  sessionId: string,
  params: ListMessagesParams = {},
): Promise<ListMessagesResult> {
  if (USE_MOCK) return mock.listMessages(sessionId, params)
  const search = new URLSearchParams()
  if (params.cursor) search.set('cursor', params.cursor)
  if (params.limit) search.set('limit', String(params.limit))
  const qs = search.toString()
  return request<ListMessagesResult>(
    `/sessions/${encodeURIComponent(sessionId)}/messages${qs ? `?${qs}` : ''}`,
    { method: 'GET' },
  )
}

async function listSessionCheckpoints(
  sessionId: string,
  limit = 20,
): Promise<SessionCheckpointsResult> {
  if (USE_MOCK) return mock.listSessionCheckpoints(sessionId, limit)
  return request<SessionCheckpointsResult>(
    `/sessions/${encodeURIComponent(sessionId)}/checkpoints?limit=${limit}`,
    { method: 'GET' },
  )
}

async function restoreCheckpoint(id: string): Promise<Record<string, unknown>> {
  if (USE_MOCK) return mock.restoreCheckpoint(id)
  return request<Record<string, unknown>>(`/checkpoints/${encodeURIComponent(id)}/restore`, {
    method: 'POST',
  })
}

async function listSessionSubagents(
  sessionId: string,
  limit = 100,
): Promise<SessionSubagentsResult> {
  if (USE_MOCK) return mock.listSessionSubagents(sessionId, limit)
  return request<SessionSubagentsResult>(
    `/sessions/${encodeURIComponent(sessionId)}/subagents?limit=${limit}`,
    { method: 'GET' },
  )
}

async function cancelSubagent(id: string): Promise<Record<string, unknown>> {
  if (USE_MOCK) return mock.cancelSubagent(id)
  return request<Record<string, unknown>>(`/subagents/${encodeURIComponent(id)}/cancel`, {
    method: 'POST',
  })
}

async function listSessionFiles(
  sessionId: string,
  path = '',
  limit = 200,
): Promise<SessionFilesResult> {
  if (USE_MOCK) return mock.listSessionFiles(sessionId, path, limit)
  const search = new URLSearchParams()
  if (path) search.set('path', path)
  search.set('limit', String(limit))
  return request<SessionFilesResult>(
    `/sessions/${encodeURIComponent(sessionId)}/files?${search.toString()}`,
    { method: 'GET' },
  )
}

function sessionFileDownloadUrl(sessionId: string, path: string): string {
  if (USE_MOCK) return '#'
  const encoded = path
    .split('/')
    .filter(Boolean)
    .map((segment) => encodeURIComponent(segment))
    .join('/')
  return `${apiBase()}/api/v1/sessions/${encodeURIComponent(sessionId)}/files/download?path=${encodeURIComponent(encoded)}`
}

async function sendMessage(
  sessionId: string,
  content: MessageInputContent,
  options: SendMessageOptions & { model?: string } = {},
): Promise<SendMessageResult> {
  if (USE_MOCK) {
    return mock.sendMessage(
      sessionId,
      { content },
      { idempotencyKey: options.idempotencyKey, model: options.model },
    )
  }
  const data = await request<SendMessageResult>(
    `/sessions/${encodeURIComponent(sessionId)}/messages`,
    {
      method: 'POST',
      body: { content, model: options.model },
      headers: { 'Idempotency-Key': options.idempotencyKey || makeIdempotencyKey() },
      signal: options.signal,
    },
  )
  if (!data?.run_id) {
    throw new ApiError(502, 'missing_run_id', '服务未返回 run_id')
  }
  return data
}

async function interrupt(sessionId: string, reason?: string): Promise<{ ok: boolean }> {
  if (USE_MOCK) {
    const result = await mock.interrupt(sessionId, reason)
    return { ok: result.cancelled }
  }
  return request<{ ok: boolean }>(`/sessions/${encodeURIComponent(sessionId)}/interrupt`, {
    method: 'POST',
    body: { reason },
  })
}

async function steer(sessionId: string, content: string): Promise<{ ok: boolean }> {
  if (USE_MOCK) {
    const result = await mock.steer(sessionId, content)
    return { ok: result.accepted }
  }
  return request<{ ok: boolean }>(`/sessions/${encodeURIComponent(sessionId)}/steer`, {
    method: 'POST',
    body: { content },
  })
}

async function resolveApproval(
  sessionId: string,
  approvalId: string,
  decision: ApprovalDecision,
  note?: string,
): Promise<{ approval_id: string; decision: ApprovalDecision }> {
  if (USE_MOCK) return mock.resolveApproval(sessionId, approvalId, decision, note)
  return request<{ approval_id: string; decision: ApprovalDecision }>(
    `/sessions/${encodeURIComponent(sessionId)}/approvals/${encodeURIComponent(approvalId)}`,
    { method: 'POST', body: { decision, note } },
  )
}

async function getRun(runId: string): Promise<RunDetailResult> {
  if (USE_MOCK) return mock.getRun(runId)
  return request<RunDetailResult>(`/runs/${encodeURIComponent(runId)}`, { method: 'GET' })
}

// ── 记忆 ────────────────────────────────────────────────────────────────

function queryString(params: Record<string, string | number | null | undefined> = {}): string {
  const search = new URLSearchParams()
  for (const [key, value] of Object.entries(params)) {
    if (value === undefined || value === null || value === '') continue
    search.set(key, String(value))
  }
  const qs = search.toString()
  return qs ? `?${qs}` : ''
}

/**
 * 部分写接口直接返回行本身（MCP / 插件 / 技能），少量包在 `{ key: row }` 里
 * （记忆）。这里两种都接受，避免前端被后端包装层差异绑死。
 */
function pickRow<T>(data: unknown, key: string): T {
  if (data && typeof data === 'object' && key in (data as Record<string, unknown>)) {
    return (data as Record<string, unknown>)[key] as T
  }
  return data as T
}

async function listMemories(params: ListMemoriesParams = {}): Promise<Memory[]> {
  if (USE_MOCK) return mock.listMemories(params)
  const data = await request<{ memories?: Memory[] }>(
    `/memories${queryString({ scope: params.scope, slot: params.slot })}`,
    { method: 'GET' },
  )
  return data?.memories ?? []
}

async function createMemory(body: CreateMemoryRequest): Promise<Memory> {
  if (USE_MOCK) return mock.createMemory(body)
  const data = await request<unknown>('/memories', { method: 'POST', body })
  return pickRow<Memory>(data, 'memory')
}

async function updateMemory(id: string, body: UpdateMemoryRequest): Promise<Memory> {
  if (USE_MOCK) return mock.updateMemory(id, body)
  const data = await request<unknown>(`/memories/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return pickRow<Memory>(data, 'memory')
}

async function deleteMemory(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteMemory(id)
  await request<Record<string, unknown>>(`/memories/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

async function clearMemories(
  params: { scope?: string; slot?: string } = {},
): Promise<ClearMemoriesResult> {
  if (USE_MOCK) return mock.clearMemories(params)
  const data = await request<{ deleted?: number }>(
    `/memories${queryString({ scope: params.scope, slot: params.slot })}`,
    { method: 'DELETE' },
  )
  return { deleted: data?.deleted ?? 0 }
}

// ── MCP ─────────────────────────────────────────────────────────────────

async function listMcpServers(): Promise<McpServer[]> {
  if (USE_MOCK) return mock.listMcpServers()
  const data = await request<{ servers?: McpServer[] }>('/mcp/servers', { method: 'GET' })
  return data?.servers ?? []
}

async function createMcpServer(body: McpServerInput): Promise<McpServer> {
  if (USE_MOCK) return mock.createMcpServer(body)
  const data = await request<unknown>('/mcp/servers', { method: 'POST', body })
  return pickRow<McpServer>(data, 'server')
}

async function updateMcpServer(id: string, body: Partial<McpServerInput>): Promise<McpServer> {
  if (USE_MOCK) return mock.updateMcpServer(id, body)
  const data = await request<unknown>(`/mcp/servers/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return pickRow<McpServer>(data, 'server')
}

async function deleteMcpServer(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteMcpServer(id)
  await request<Record<string, unknown>>(`/mcp/servers/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

async function probeMcpServer(id: string): Promise<McpProbeResult> {
  if (USE_MOCK) return mock.probeMcpServer(id)
  const data = await request<McpProbeResult>(`/mcp/servers/${encodeURIComponent(id)}/probe`, {
    method: 'POST',
  })
  return {
    protocol_version: data?.protocol_version,
    server_info: data?.server_info,
    tools: Array.isArray(data?.tools) ? data.tools : [],
  }
}

async function listMcpTools(): Promise<McpTool[]> {
  if (USE_MOCK) return mock.listMcpTools()
  const data = await request<{ tools?: McpTool[] }>('/mcp/tools', { method: 'GET' })
  return data?.tools ?? []
}

// ── 插件 ────────────────────────────────────────────────────────────────

async function listPlugins(): Promise<Plugin[]> {
  if (USE_MOCK) return mock.listPlugins()
  const data = await request<{ plugins?: Plugin[] }>('/plugins', { method: 'GET' })
  return data?.plugins ?? []
}

async function createPlugin(body: CreatePluginRequest): Promise<Plugin> {
  if (USE_MOCK) return mock.createPlugin(body)
  const data = await request<unknown>('/plugins', { method: 'POST', body })
  return pickRow<Plugin>(data, 'plugin')
}

async function updatePlugin(id: string, body: UpdatePluginRequest): Promise<Plugin> {
  if (USE_MOCK) return mock.updatePlugin(id, body)
  const data = await request<unknown>(`/plugins/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return pickRow<Plugin>(data, 'plugin')
}

async function deletePlugin(id: string): Promise<void> {
  if (USE_MOCK) return mock.deletePlugin(id)
  await request<Record<string, unknown>>(`/plugins/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

// ── 技能 ────────────────────────────────────────────────────────────────

async function listSkills(): Promise<Skill[]> {
  if (USE_MOCK) return mock.listSkills()
  const data = await request<{ skills?: Skill[] }>('/skills', { method: 'GET' })
  return data?.skills ?? []
}

async function createSkill(body: CreateSkillRequest): Promise<Skill> {
  if (USE_MOCK) return mock.createSkill(body)
  const data = await request<unknown>('/skills', { method: 'POST', body })
  return pickRow<Skill>(data, 'skill')
}

async function updateSkill(id: string, body: UpdateSkillRequest): Promise<Skill> {
  if (USE_MOCK) return mock.updateSkill(id, body)
  const data = await request<unknown>(`/skills/${encodeURIComponent(id)}`, {
    method: 'PATCH',
    body,
  })
  return pickRow<Skill>(data, 'skill')
}

async function deleteSkill(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteSkill(id)
  await request<Record<string, unknown>>(`/skills/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

// ── 市场 ────────────────────────────────────────────────────────────────

async function listMarketItems(params: { kind?: 'plugin' | 'skill' } = {}): Promise<MarketItem[]> {
  if (USE_MOCK) return mock.listMarketItems(params)
  const data = await request<{ items?: MarketItem[] }>(
    `/market/items${queryString({ kind: params.kind })}`,
    { method: 'GET' },
  )
  return data?.items ?? []
}

async function installMarketItem(
  itemId: string,
  scope?: 'user' | 'tenant',
): Promise<{ installed: boolean; item_id: string }> {
  if (USE_MOCK) return mock.installMarketItem(itemId, scope)
  const data = await request<{ item_id?: string } | null>(
    `/market/items/${encodeURIComponent(itemId)}/install`,
    { method: 'POST', body: { scope: scope ?? 'user' } },
  )
  // 后端成功即代表已落成 plugins/skills 行，`installed` 由这里统一给出。
  return { installed: true, item_id: data?.item_id ?? itemId }
}

async function uninstallMarketItem(
  itemId: string,
  scope?: 'user' | 'tenant',
): Promise<UninstallMarketItemResult> {
  if (USE_MOCK) return mock.uninstallMarketItem(itemId, scope)
  const data = await request<{ removed?: number } | null>(
    `/market/items/${encodeURIComponent(itemId)}/uninstall`,
    { method: 'POST', body: { scope: scope ?? 'user' } },
  )
  return { removed: data?.removed ?? 0 }
}

// ── 成员（owner/admin） ─────────────────────────────────────────────────

async function listUsers(params: ListUsersParams = {}): Promise<ListUsersResult> {
  if (USE_MOCK) return mock.listUsers(params)
  const page = params.page ?? 1
  const perPage = params.per_page ?? 20
  const data = await request<Partial<ListUsersResult>>(
    `/admin/users${queryString({ page, per_page: perPage })}`,
    { method: 'GET' },
  )
  return {
    users: data?.users ?? [],
    total: data?.total ?? 0,
    page: data?.page ?? page,
    per_page: data?.per_page ?? perPage,
  }
}

async function createUser(body: CreateUserRequest): Promise<CreateUserResult> {
  if (USE_MOCK) return mock.createUser(body)
  const data = await request<{ user: User; initial_password?: string }>('/admin/users', {
    method: 'POST',
    body,
  })
  return { user: data.user, initial_password: data.initial_password ?? '' }
}

async function resetUserPassword(id: string): Promise<ResetUserPasswordResult> {
  if (USE_MOCK) return mock.resetUserPassword(id)
  const data = await request<{ temporary_password?: string }>(
    `/admin/users/${encodeURIComponent(id)}/password/reset`,
    { method: 'POST' },
  )
  return { temporary_password: data?.temporary_password ?? '' }
}

async function disableUser(id: string): Promise<void> {
  if (USE_MOCK) return mock.disableUser(id)
  await request<Record<string, unknown>>(`/admin/users/${encodeURIComponent(id)}/disable`, {
    method: 'POST',
  })
}

async function enableUser(id: string): Promise<void> {
  if (USE_MOCK) return mock.enableUser(id)
  await request<Record<string, unknown>>(`/admin/users/${encodeURIComponent(id)}/enable`, {
    method: 'POST',
  })
}

async function forceLogout(id: string): Promise<void> {
  if (USE_MOCK) return mock.forceLogout(id)
  await request<Record<string, unknown>>(`/admin/users/${encodeURIComponent(id)}/force-logout`, {
    method: 'POST',
  })
}

async function deleteUser(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteUser(id)
  await request<Record<string, unknown>>(`/admin/users/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  })
}

// ── 导出 ────────────────────────────────────────────────────────────────

// ── 用量 / 审计（owner/admin） ─────────────────────────────────────────

async function getUsage(params: UsageParams = {}): Promise<UsageReport> {
  if (USE_MOCK) return mock.getUsage(params)
  const search = new URLSearchParams()
  if (params.days !== undefined) search.set('days', String(params.days))
  if (params.from) search.set('from', params.from)
  if (params.to) search.set('to', params.to)
  if (params.user_id) search.set('user_id', params.user_id)
  const qs = search.toString()
  return request<UsageReport>(`/admin/usage${qs ? `?${qs}` : ''}`, { method: 'GET' })
}

async function listAudit(params: AuditParams = {}): Promise<ListAuditResult> {
  if (USE_MOCK) return mock.listAudit(params)
  const search = new URLSearchParams()
  if (params.action) search.set('action', params.action)
  if (params.user_id) search.set('user_id', params.user_id)
  if (params.from) search.set('from', params.from)
  if (params.to) search.set('to', params.to)
  if (params.cursor) search.set('cursor', params.cursor)
  if (params.limit) search.set('limit', String(params.limit))
  const qs = search.toString()
  return request<ListAuditResult>(`/admin/audit${qs ? `?${qs}` : ''}`, { method: 'GET' })
}

// ── 媒体 / 文件库 ───────────────────────────────────────────────────────

function mediaUrl(id: string, download = false): string {
  if (USE_MOCK) return mock.mediaUrl(id)
  return `${apiBase()}/api/v1/media/${encodeURIComponent(id)}${download ? '?download=1' : ''}`
}

async function listMedia(params: ListMediaParams = {}): Promise<ListMediaResult> {
  if (USE_MOCK) return mock.listMedia(params)
  const search = new URLSearchParams()
  if (params.cursor) search.set('cursor', params.cursor)
  if (params.limit) search.set('limit', String(params.limit))
  const qs = search.toString()
  return request<ListMediaResult>(`/media${qs ? `?${qs}` : ''}`, { method: 'GET' })
}

async function uploadMedia(
  file: Blob,
  filename?: string,
  options: { signal?: AbortSignal } = {},
): Promise<MediaAsset> {
  if (USE_MOCK) {
    if (options.signal?.aborted) throw new DOMException('Aborted', 'AbortError')
    return mock.uploadMedia(file, filename)
  }
  const headers = new Headers()
  headers.set('Content-Type', file.type || 'application/octet-stream')
  const token = getToken()
  if (token) headers.set('Authorization', `Bearer ${token}`)
  const name = filename ?? (file instanceof File ? file.name : 'upload.bin')
  const res = await fetch(
    `${apiBase()}/api/v1/media?filename=${encodeURIComponent(name)}`,
    {
      method: 'POST',
      headers,
      body: file,
      credentials: 'include',
      cache: 'no-store',
      signal: options.signal,
    },
  )
  const body = await res.json().catch(() => ({}))
  if (!res.ok) {
    const message = body?.error?.message || `HTTP ${res.status}`
    throw new ApiError(res.status, body?.error?.code ?? 'upload_failed', message)
  }
  return (body?.data?.media ?? body?.data) as MediaAsset
}

async function deleteMedia(id: string): Promise<void> {
  if (USE_MOCK) return mock.deleteMedia(id)
  await request(`/media/${encodeURIComponent(id)}`, { method: 'DELETE' })
}

export interface ApiClient {
  login: typeof login
  logout: typeof logout
  session: typeof session
  changePassword: typeof changePassword
  profile: typeof profile
  updateProfile: typeof updateProfile
  listModels: typeof listModels
  adminModels: typeof adminModels
  addProviderKey: typeof addProviderKey
  updateProviderKey: typeof updateProviderKey
  deleteProviderKey: typeof deleteProviderKey
  setProviderEnabled: typeof setProviderEnabled
  addCatalogModel: typeof addCatalogModel
  updateCatalogModel: typeof updateCatalogModel
  deleteCatalogModel: typeof deleteCatalogModel
  addModelRoute: typeof addModelRoute
  updateModelRoute: typeof updateModelRoute
  deleteModelRoute: typeof deleteModelRoute
  discoverModels: typeof discoverModels
  probeProvider: typeof probeProvider
  listTenantModels: typeof listTenantModels
  getHiddenModels: typeof getHiddenModels
  setHiddenModels: typeof setHiddenModels
  listSessions: typeof listSessions
  createSession: typeof createSession
  getSession: typeof getSession
  updateSession: typeof updateSession
  deleteSession: typeof deleteSession
  listMessages: typeof listMessages
  listSessionCheckpoints: typeof listSessionCheckpoints
  restoreCheckpoint: typeof restoreCheckpoint
  listSessionSubagents: typeof listSessionSubagents
  cancelSubagent: typeof cancelSubagent
  listSessionFiles: typeof listSessionFiles
  sessionFileDownloadUrl: typeof sessionFileDownloadUrl
  sendMessage: typeof sendMessage
  interrupt: typeof interrupt
  steer: typeof steer
  resolveApproval: typeof resolveApproval
  getRun: typeof getRun
  listMemories: typeof listMemories
  createMemory: typeof createMemory
  updateMemory: typeof updateMemory
  deleteMemory: typeof deleteMemory
  clearMemories: typeof clearMemories
  listMcpServers: typeof listMcpServers
  createMcpServer: typeof createMcpServer
  updateMcpServer: typeof updateMcpServer
  deleteMcpServer: typeof deleteMcpServer
  probeMcpServer: typeof probeMcpServer
  listMcpTools: typeof listMcpTools
  listPlugins: typeof listPlugins
  createPlugin: typeof createPlugin
  updatePlugin: typeof updatePlugin
  deletePlugin: typeof deletePlugin
  listSkills: typeof listSkills
  createSkill: typeof createSkill
  updateSkill: typeof updateSkill
  deleteSkill: typeof deleteSkill
  listMarketItems: typeof listMarketItems
  installMarketItem: typeof installMarketItem
  uninstallMarketItem: typeof uninstallMarketItem
  listUsers: typeof listUsers
  createUser: typeof createUser
  resetUserPassword: typeof resetUserPassword
  disableUser: typeof disableUser
  enableUser: typeof enableUser
  forceLogout: typeof forceLogout
  deleteUser: typeof deleteUser
  getUsage: typeof getUsage
  listAudit: typeof listAudit
  listMedia: typeof listMedia
  uploadMedia: typeof uploadMedia
  deleteMedia: typeof deleteMedia
  mediaUrl: typeof mediaUrl
}

export const api: ApiClient = {
  login,
  logout,
  session,
  changePassword,
  profile,
  updateProfile,
  listModels,
  adminModels,
  addProviderKey,
  updateProviderKey,
  deleteProviderKey,
  setProviderEnabled,
  addCatalogModel,
  updateCatalogModel,
  deleteCatalogModel,
  addModelRoute,
  updateModelRoute,
  deleteModelRoute,
  discoverModels,
  probeProvider,
  listTenantModels,
  getHiddenModels,
  setHiddenModels,
  listSessions,
  createSession,
  getSession,
  updateSession,
  deleteSession,
  listMessages,
  listSessionCheckpoints,
  restoreCheckpoint,
  listSessionSubagents,
  cancelSubagent,
  listSessionFiles,
  sessionFileDownloadUrl,
  sendMessage,
  interrupt,
  steer,
  resolveApproval,
  getRun,
  listMemories,
  createMemory,
  updateMemory,
  deleteMemory,
  clearMemories,
  listMcpServers,
  createMcpServer,
  updateMcpServer,
  deleteMcpServer,
  probeMcpServer,
  listMcpTools,
  listPlugins,
  createPlugin,
  updatePlugin,
  deletePlugin,
  listSkills,
  createSkill,
  updateSkill,
  deleteSkill,
  listMarketItems,
  installMarketItem,
  uninstallMarketItem,
  listUsers,
  createUser,
  resetUserPassword,
  disableUser,
  enableUser,
  forceLogout,
  deleteUser,
  getUsage,
  listAudit,
  listMedia,
  uploadMedia,
  deleteMedia,
  mediaUrl,
}

/** 会话标题展示兜底（新建会话服务端可能给空标题）。 */
export function sessionTitle(session: Pick<ChatSession, 'title'> | null | undefined): string {
  const title = session?.title?.trim()
  return title || '未命名会话'
}

export type { ListSessionsResult }
