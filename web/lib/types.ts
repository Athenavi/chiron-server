/**
 * chiron-web 的共享类型与错误。
 *
 * 约定（与 chiron-server 一致）：
 * - 成功响应统一包在 `{ data: ... }` 里，失败是 `{ error: { code, message } }`；
 * - 时间字段是 RFC3339 字符串（UTC）；
 * - 会话事件 envelope 是 `{ id, seq, run_id?, type, data, ts }`，SSE 的 `id:` 是 `seq`。
 *
 * 这是共享契约文件：页面与组件只依赖 `lib/types.ts` / `lib/api.ts` / `lib/sse.ts`。
 */

import { uiText } from './ui-locale'

// ── 错误 ────────────────────────────────────────────────────────────────

/** 统一的 API 错误：HTTP 状态 + 后端错误码 + 可展示的中文/英文消息。 */
export class ApiError extends Error {
  readonly status: number
  readonly code: string

  constructor(status: number, code: string, message: string) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.code = code
  }

  static from(
    error: unknown,
    fallbackMessage = uiText('请求失败，请稍后重试', 'Request failed. Please try again.'),
  ): ApiError {
    if (error instanceof ApiError) return error
    if (error instanceof Error) {
      if (error.name === 'AbortError') {
        return new ApiError(0, 'aborted', uiText('请求已取消', 'Request cancelled'))
      }
      return new ApiError(0, 'network_error', error.message || fallbackMessage)
    }
    return new ApiError(0, 'unknown_error', fallbackMessage)
  }
}

export function isApiError(error: unknown): error is ApiError {
  return error instanceof ApiError
}

/** `{ error: { code, message } }` 响应体。 */
export interface ApiErrorBody {
  error: { code: string; message: string }
}

/** `{ data: T }` 响应体。 */
export interface ApiEnvelope<T> {
  data: T
}

// ── 用户 / 认证 ─────────────────────────────────────────────────────────

export type UserRole = string
export type UserStatus = 'active' | 'disabled' | 'deleted' | string

export interface User {
  id: string
  email: string
  username: string | null
  name: string
  nickname: string
  role: UserRole
  status: UserStatus
  must_change_password: boolean
  password_set: boolean
  last_login_at: string | null
}

/** 套餐配额（`plan` 可能为 null，例如未绑定套餐的本地部署）。 */
export interface Plan {
  plan_id: string
  max_users: number
  max_concurrent_runs: number
  monthly_tokens: number
  max_storage_gb: number
  max_sandboxes: number
}

export interface AuthPayload {
  token: string
  user: User
  plan: Plan | null
}

export interface LoginRequest {
  identifier: string
  password: string
}

export type LoginResult = AuthPayload
export type SessionResult = AuthPayload
export type ChangePasswordResult = { token: string; user: User }
export type UpdateProfileRequest = { nickname?: string; name?: string }

// ── 模型目录 ────────────────────────────────────────────────────────────

export type ProviderProtocol = 'openai' | 'anthropic' | string

export interface ModelProvider {
  provider: string
  label: string
  protocol: ProviderProtocol
  base_url: string | null
  model_prefixes: string[]
  requires_key: boolean
  model_discovery: boolean
}

export interface ModelCatalog {
  providers: ModelProvider[]
}

/**
 * 租户模型服务（管理端）。
 *
 * - `providers` 是 provider 级开关与密钥计数；
 * - `keys` 是租户下的 API Key 池，`weight` 用于同 provider 内加权轮询；
 * - `models` 是租户目录（是否对用户可见由 `enabled` 控制）；
 * - `routes` 把目录模型映射到 provider 的 upstream 模型，支持优先级与权重。
 */
export interface AdminModelProvider {
  provider: string
  label: string
  keys_count: number
  enabled: boolean
}

export interface ModelProviderKey {
  id: string
  provider: string
  label: string
  weight: number
  enabled: boolean
  updated_at: string
}

/** 模型来源：手工录入或由 Provider 自动发现写入。 */
export type CatalogModelSource = 'manual' | 'discovered'

export interface CatalogModel {
  model: string
  display_name: string
  enabled: boolean
  created_at: string
  /** 缺省视为 manual（旧后端不带该字段）。 */
  source?: CatalogModelSource
  discovered_at?: string | null
}

export interface ModelRoute {
  id: string
  model: string
  provider: string
  upstream_model: string
  priority: number
  weight: number
  enabled: boolean
  /** 最近一次成功调用时间；从未成功为 null。 */
  last_ok_at?: string | null
  /** 最近一次失败原因；成功后由后端清空。 */
  last_error?: string
  /** 最近一次健康探测时间。 */
  last_probe_at?: string | null
}

/** 自动发现：单个 provider 的探测结果。 */
export interface DiscoveredModel {
  /** upstream 侧模型名（不带 `provider:` 前缀）。 */
  model: string
  /** 是否已在租户目录中。 */
  exists: boolean
}

export interface DiscoverProviderResult {
  provider: string
  ok: boolean
  models: DiscoveredModel[]
  error?: string
}

export interface DiscoverModelsResult {
  results: DiscoverProviderResult[]
}

export interface DiscoverModelsRequest {
  /** 只发现指定 provider；缺省发现全部。 */
  provider?: string
  /** true 时把发现的模型写入目录并生成路由。 */
  persist?: boolean
}

/** 健康探测结果。 */
export interface ProviderProbeResult {
  ok: boolean
  /** 探测到的模型数量。 */
  models?: number
  error?: string
}

export interface AdminModelsResult {
  providers: AdminModelProvider[]
  keys: ModelProviderKey[]
  models: CatalogModel[]
  routes: ModelRoute[]
}

/** 普通用户可见的目录模型（`providers` 为当前可用路由的 provider 列表）。 */
export interface TenantModel {
  model: string
  display_name: string
  providers: string[]
}

export interface TenantModelsResult {
  models: TenantModel[]
  /** 兼容旧契约：provider preset 清单。 */
  providers: ModelProvider[]
}

export interface HiddenModelsResult {
  models: string[]
}

export interface AddProviderKeyRequest {
  provider: string
  api_key: string
  label?: string
  weight?: number
}

export interface UpdateProviderKeyRequest {
  enabled?: boolean
  weight?: number
}

export interface AddCatalogModelRequest {
  model: string
  display_name?: string
}

export interface UpdateCatalogModelRequest {
  enabled?: boolean
  display_name?: string
}

export interface AddModelRouteRequest {
  model: string
  provider: string
  upstream_model?: string
  priority?: number
  weight?: number
}

export interface UpdateModelRouteRequest {
  enabled?: boolean
  upstream_model?: string
  priority?: number
  weight?: number
}

// ── 会话 ────────────────────────────────────────────────────────────────

export type SessionStatus = 'active' | 'archived' | 'deleted' | string
export type SessionFilter = 'active' | 'archived' | 'all'

export interface ChatSession {
  id: string
  tenant_id: number
  user_id: string
  parent_session_id: string | null
  title: string
  project_id: string | null
  model: string | null
  status: SessionStatus
  created_at: string
  updated_at: string
  archived_at: string | null
  deleted_at: string | null
  /** 列表接口附带：当前会话的活跃 run（没有为 null）。 */
  active_run?: ActiveRun | null
  /** 列表接口附带：等待处理的审批数。 */
  pending_approvals?: number
}

export interface ListSessionsParams {
  status?: SessionFilter
  cursor?: string | null
  limit?: number
}

export interface ListSessionsResult {
  sessions: ChatSession[]
  next_cursor: string | null
  /** 当前用户可见范围内所有待审批数（不限于本页）。 */
  pending_approvals_total?: number
}

export interface CreateSessionRequest {
  title?: string
  model?: string
  project_id?: string
  parent_session_id?: string
}

export interface CreateSessionResult {
  session: ChatSession
}

/** PATCH 的线上字段是 `archived: boolean`；同时兼容 `status` 写法。 */
export interface UpdateSessionRequest {
  title?: string
  model?: string
  status?: 'active' | 'archived'
  archived?: boolean
}

export interface UpdateSessionResult {
  session: ChatSession
}

export interface ActiveRun {
  run_id: string
  status: RunStatus
}

export interface SessionDetailResult {
  session: ChatSession
  active_run: ActiveRun | null
}

// ── 检查点 / 子 Agent ───────────────────────────────────────────────────

export interface CheckpointSummary {
  id: string
  run_id: string
  session_id: string
  seq: number
  kind: string
  label: string
  status: string
  snapshot?: unknown
  workspace_commit: string | null
  workspace_ref: string | null
  size_bytes: number
  created_at: string
  expires_at: string
}

export interface SubagentSummary {
  id: string
  parent_run_id: string
  root_run_id: string
  parent_subagent_id: string | null
  session_id: string
  depth: number
  agent: string
  task: string
  status: string
  model: string | null
  budget: unknown
  usage: unknown
  summary: string | null
  error: unknown
  created_at: string
  updated_at: string
  started_at: string | null
  finished_at: string | null
}

export interface SessionCheckpointsResult {
  session_id?: string
  checkpoints: CheckpointSummary[]
  count?: number
}

export interface SessionSubagentsResult {
  session_id?: string
  subagents: SubagentSummary[]
  count?: number
}

export interface SessionFileEntry {
  name: string
  kind: 'file' | 'dir' | 'symlink' | string
  size: number
  modified_at: string | null
  path: string
}

export interface SessionFilesResult {
  path: string
  entries: SessionFileEntry[]
  truncated: boolean
}

// ── 消息 ────────────────────────────────────────────────────────────────

export type MessageRole = 'user' | 'assistant' | 'system' | 'tool' | string
export type MessageKind = 'chat' | 'tool' | 'event' | 'error' | string

export interface ToolCall {
  id?: string
  tool_call_id?: string
  name?: string
  args?: unknown
  arguments?: unknown
  status?: string
  result?: unknown
  error?: string | null
}

/** `content` 可能是纯文本、`{ text, tool_calls? }` 或内容块数组。 */
export type MessageContent =
  | string
  | null
  | { text?: string; tool_calls?: ToolCall[] | null; [key: string]: unknown }
  | Array<{ type?: string; text?: string; [key: string]: unknown }>

export interface ChatMessage {
  id: string
  session_id: string
  turn_id: string | null
  run_id: string | null
  role: MessageRole
  kind: MessageKind
  content: MessageContent
  created_at: string
}

export interface ListMessagesParams {
  cursor?: string | null
  limit?: number
}

export interface ListMessagesResult {
  messages: ChatMessage[]
  next_cursor: string | null
}

/** 发送体：字符串或 `{ type: "text", text }`（服务端两种都接受）。 */
export type MessageInputContent = string | { type: 'text'; text: string }

export interface SendMessageRequest {
  content: MessageInputContent
  model?: string
  client_id?: string
}

export interface SendMessageResult {
  session_id: string
  message_id: string
  run_id: string
  turn_id?: string
  status?: string
  model?: string | null
}

export interface SendMessageOptions {
  /** 幂等键；不传则自动生成（同一次点击重试应复用同一个）。 */
  idempotencyKey?: string
  signal?: AbortSignal
}

// ── Run / 审批 / 用量 ───────────────────────────────────────────────────

export type RunStatus =
  | 'queued'
  | 'running'
  | 'waiting_approval'
  | 'waiting_user'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | string

export interface RunUsage {
  input_tokens?: number
  output_tokens?: number
  cost?: number
  [key: string]: unknown
}

export interface Run {
  id: string
  session_id: string
  turn_id: string | null
  status: RunStatus
  error: { code?: string; message?: string; reason?: string } | null
  usage: RunUsage
  created_at: string
  updated_at: string
  started_at: string | null
  finished_at: string | null
  lease_owner: string | null
  lease_until: string | null
}

export interface Turn {
  id: string
  status: string
  model: string | null
  usage: RunUsage
}

export interface RunDetailResult {
  run: Run
  turn: Turn | null
}

export type ApprovalDecision = 'allow' | 'deny'

export interface ResolveApprovalRequest {
  decision: ApprovalDecision
  note?: string
}

// ── SSE 事件 ────────────────────────────────────────────────────────────

export interface RunStartedData {
  model?: string | null
  turn_id?: string
}

export interface MessageDeltaData {
  message_id: string
  delta: string
}

export interface ReasoningDeltaData {
  delta: string
}

export interface MessageCompletedData {
  message_id: string
  content?: MessageContent
  usage?: RunUsage
  tool_calls?: ToolCall[] | null
}

export interface RunCompletedData {
  status?: 'completed'
  usage?: RunUsage
}

export interface RunFailedData {
  code?: string
  message?: string
}

export interface RunCancelledData {
  reason?: string
}

export interface ToolCallData {
  tool_call_id: string
  name: string
  args?: unknown
}

export interface ToolResultData {
  tool_call_id: string
  status?: string
  result?: unknown
  error?: string | null
}

export interface ApprovalRequiredData {
  approval_id: string
  tool_call_id: string
  name: string
  args?: unknown
  expires_at?: string
}

export interface ApprovalResolvedData {
  approval_id: string
  decision: ApprovalDecision | string
}

export interface UsageUpdatedData {
  input_tokens?: number
  output_tokens?: number
  cost?: number
}

export interface HeartbeatData {
  seq?: number
}

/** `run.gap`：回放窗口丢失，客户端应重拉消息快照。 */
export interface RunGapData {
  from?: number
  to?: number
  reason?: string
}

export interface EventDataMap {
  'run.started': RunStartedData
  'message.delta': MessageDeltaData
  'reasoning.delta': ReasoningDeltaData
  'message.completed': MessageCompletedData
  'run.completed': RunCompletedData
  'run.failed': RunFailedData
  'run.cancelled': RunCancelledData
  'tool.call': ToolCallData
  'tool.result': ToolResultData
  'approval.required': ApprovalRequiredData
  'approval.resolved': ApprovalResolvedData
  'usage.updated': UsageUpdatedData
  heartbeat: HeartbeatData
  'run.gap': RunGapData
}

export type SessionEventType = keyof EventDataMap

/** 服务端 envelope；`type` 超出已知集合时按未知事件透传（data 为任意对象）。 */
export type SessionEvent = {
  [K in SessionEventType]: {
    /** 全局唯一，客户端按它去重。 */
    id: string
    /** 会话内单调递增；SSE `id:` 字段，也是 `Last-Event-ID`。 */
    seq: number
    run_id?: string | null
    type: K
    data: EventDataMap[K]
    ts?: string
  }
}[SessionEventType]

/** 订阅回调。 */
export type SessionEventMap = { [K in SessionEventType]: EventDataMap[K] }

export interface SessionEventHandlers {
  onEvent: (event: SessionEvent) => void
  /** 收到 `run.gap` 时额外回调，上层应重拉消息。 */
  onGap?: () => void
  onError?: (error: ApiError) => void
  onOpen?: () => void
}

export interface SubscribeOptions {
  /** 断线重连时带给服务端的最后一个 seq。 */
  lastEventId?: string | number | null
  /** 页面隐藏时暂停（浏览器标签不活跃时省流）。 */
  pauseWhenHidden?: boolean
}

export type Unsubscribe = () => void

// ── 记忆（`/api/v1/memories`） ──────────────────────────────────────────

/** 槽位：persona / preferences / facts / summary。 */
export type MemorySlot = 'persona' | 'preferences' | 'facts' | 'summary'

/**
 * 一条记忆。
 *
 * `scope` 是 `global`（跨会话）或 `session:<uuid>`（会话级）；`source` 目前是
 * `user` / `auto` / `import`。
 */
export interface Memory {
  id: string
  tenant_id: number
  user_id: string
  scope: string
  slot: MemorySlot
  content: string
  source: string
  confidence: number
  session_id: string | null
  created_at: string
  updated_at: string
}

export interface ListMemoriesParams {
  scope?: string
  slot?: string
}

export interface CreateMemoryRequest {
  slot: MemorySlot
  content: string
  scope?: string
  session_id?: string
  source?: string
  confidence?: number
}

export interface UpdateMemoryRequest {
  content?: string
  slot?: MemorySlot
  source?: string
  confidence?: number
}

export interface ListMemoriesResult {
  memories: Memory[]
}

export interface CreateMemoryResult {
  memory: Memory
}

export interface UpdateMemoryResult {
  memory: Memory
}

export interface DeleteMemoryResult {
  id: string
  deleted: true
}

export interface ClearMemoriesResult {
  deleted: number
}

// ── MCP（`/api/v1/mcp/*`） ──────────────────────────────────────────────

export type McpTransport = 'stdio' | 'sse' | 'http'

/**
 * 已发现的 MCP 工具。
 *
 * 后端两处序列化字段名不一致（探测结果用 `inputSchema`，汇总列表用
 * `input_schema`），这里两个都声明为可选，并保留索引签名以免过度约束。
 */
export interface McpTool {
  name: string
  description?: string | null
  input_schema?: unknown
  inputSchema?: unknown
  /** 汇总列表里的来源 server（`GET /mcp/tools`）。 */
  server?: string
  /** 汇总列表里的全限定名 `mcp__<server>__<tool>`。 */
  qualified_name?: string
  [key: string]: unknown
}

export interface McpServer {
  id: string
  tenant_id: number
  owner_user_id: string | null
  name: string
  transport: McpTransport | string
  command: string
  args: string[]
  env: Record<string, string>
  url: string
  headers: Record<string, string>
  tool_allowlist: string[]
  enabled: boolean
  protocol_version: string | null
  server_info: unknown
  tools: McpTool[]
  last_probe_at: string | null
  last_error: string
  created_at: string
  updated_at: string
}

/** 创建/更新 MCP server 的请求体（PATCH 时全部可选）。 */
export interface McpServerInput {
  name?: string
  transport?: McpTransport | string
  command?: string
  args?: string[]
  env?: Record<string, string>
  url?: string
  headers?: Record<string, string>
  tool_allowlist?: string[]
  enabled?: boolean
  /** true = 租户共享（仅 owner/admin）。 */
  shared?: boolean
}

export interface ListMcpServersResult {
  servers: McpServer[]
}

export interface McpProbeResult {
  protocol_version?: string
  server_info?: unknown
  tools: McpTool[]
}

export interface ListMcpToolsResult {
  tools: McpTool[]
}

// ── 插件（`/api/v1/plugins`） ───────────────────────────────────────────

export interface Plugin {
  id: string
  tenant_id: number
  owner_user_id: string | null
  name: string
  kind: string
  version: string
  source: string
  builtin: boolean
  mcp_server_id: string | null
  manifest: Record<string, unknown>
  enabled: boolean
  created_at: string
  updated_at: string
}

export interface CreatePluginRequest {
  manifest: Record<string, unknown>
  mcp_server_id?: string
  shared?: boolean
  enabled?: boolean
}

export interface UpdatePluginRequest {
  enabled?: boolean
  /** 工具白名单，后端写回 `manifest.tools`。 */
  tools?: string[]
}

export interface ListPluginsResult {
  plugins: Plugin[]
}

// ── 技能（`/api/v1/skills`） ────────────────────────────────────────────

export interface Skill {
  id: string
  tenant_id: number
  owner_user_id: string | null
  name: string
  version: string
  description: string
  manifest: Record<string, unknown>
  prompt: string
  /** 资源清单（JSON，形如 `{ path, description?, content? }[]`）。 */
  resources: unknown
  /** 脚本清单（JSON，形如 `{ name, path, language, description?, args? }[]`）。 */
  scripts: unknown
  source: string
  enabled: boolean
  created_at: string
  updated_at: string
}

export interface CreateSkillRequest {
  manifest: Record<string, unknown>
  shared?: boolean
  enabled?: boolean
}

export interface UpdateSkillRequest {
  enabled?: boolean
  manifest?: Record<string, unknown>
}

export interface ListSkillsResult {
  skills: Skill[]
}

// ── 市场（`/api/v1/market/*`） ──────────────────────────────────────────

export type MarketItemKind = 'plugin' | 'skill'
export type MarketScope = 'user' | 'tenant'

export interface MarketItem {
  id: string
  item_id: string
  name: string
  kind: MarketItemKind
  version: string
  description: string
  source: string
  source_url: string
  checksum: string
  signed: boolean
  published_at: string
}

export interface ListMarketItemsParams {
  kind?: MarketItemKind
}

export interface ListMarketItemsResult {
  items: MarketItem[]
}

export interface InstallMarketItemResult {
  installed: boolean
  item_id: string
  /** 后端还会回 `kind` / `name` / `scope`，页面可按需读取。 */
  kind?: MarketItemKind | string
  name?: string
  scope?: MarketScope | string
}

export interface UninstallMarketItemResult {
  removed: number
}

// ── 成员管理（`/api/v1/admin/users`） ───────────────────────────────────

export type AdminUserRole = 'user' | 'admin' | 'owner'
export type AdminUserStatus = 'active' | 'disabled' | 'deleted'

export interface ListUsersParams {
  page?: number
  per_page?: number
}

export interface ListUsersResult {
  users: User[]
  total: number
  page: number
  per_page: number
}

export interface CreateUserRequest {
  email: string
  username?: string
  nickname?: string
  name?: string
  role?: AdminUserRole | string
  /** 留空则由服务端生成临时密码。 */
  initial_password?: string
}

export interface CreateUserResult {
  user: User
  initial_password: string
}

export interface ResetUserPasswordResult {
  temporary_password: string
}

// ── 用量 / 审计（owner/admin，`/api/v1/admin/*`） ──────────────────────

export interface UsageParams {
  /** 默认窗口（服务端缺省 7 天，1..90 截断）。 */
  days?: number
  from?: string
  to?: string
  user_id?: string
}

export interface UsageTotals {
  runs: number
  sessions: number
  input_tokens: number
  output_tokens: number
  total_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  cost: number
}

export interface UsageModelRow {
  model: string
  runs: number
  sessions: number
  input_tokens: number
  output_tokens: number
  cache_read_input_tokens: number
  cache_creation_input_tokens: number
  cost: number
}

export interface UsageUserRow {
  user_id: string
  email?: string | null
  nickname?: string | null
  runs: number
  sessions: number
  input_tokens: number
  output_tokens: number
  cost: number
}

export interface UsageToolRow {
  name: string
  calls: number
  failures: number
  total_ms: number
  avg_ms: number
  max_ms: number
}

export interface UsageReport {
  window: { from: string; to: string }
  totals: UsageTotals
  by_model: UsageModelRow[]
  by_user: UsageUserRow[]
  tools: { totals: Omit<UsageToolRow, 'name'>; by_tool: UsageToolRow[] }
}

export interface AuditParams {
  action?: string
  user_id?: string
  from?: string
  to?: string
  cursor?: string
  limit?: number
}

export interface AuditEntry {
  id: number
  tenant_id: number
  user_id?: string | null
  action: string
  target: string
  detail: string
  ip: string
  created_at: string
}

export interface ListAuditResult {
  audit: AuditEntry[]
  next_cursor: string | null
}

// ── 媒体 / 文件库（网盘） ──────────────────────────────────────────────

export type MediaKind = 'image' | 'file'

export interface MediaAsset {
  id: string
  filename: string
  mime: string
  kind: MediaKind
  bytes: number
  sha256: string
  created_at: string
  /** 同源访问地址（mock 模式可能是 object/data URL）。 */
  url: string
}

export interface ListMediaParams {
  cursor?: string
  limit?: number
}

export interface ListMediaResult {
  media: MediaAsset[]
  next_cursor: string | null
}
