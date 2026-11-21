/**
 * 内存 mock 后端。
 *
 * 只在 `NEXT_PUBLIC_USE_MOCK=1`（默认）时被 `lib/api.ts` / `lib/sse.ts` 调用，
 * 接口签名与真实 REST 一一对应，页面代码不需要感知传输层。
 *
 * - 数据存在模块内存里：页面刷新即重置；
 * - 登录：任意邮箱 + 任意密码；`demo@example.com` / `Demo!2026` 是固定演示账号；
 *   标识里含 `reset` 或 `new` 时返回 `must_change_password: true`（首次登录改密场景）；
 * - 发送消息后按 `run.started → message.delta* → message.completed → tool.call →
 *   tool.result → (approval.required) → usage.updated → run.completed` 流式产出事件，
 *   每条 delta 间隔 20–40ms；审批是否触发用 `configureMock({ approvalMode })` 控制。
 */

import {
  ApiError,
  type ApprovalDecision,
  type AuthPayload,
  type ChangePasswordResult,
  type ChatMessage,
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
  type DiscoverProviderResult,
  type EventDataMap,
  type ListMemoriesParams,
  type ListMessagesParams,
  type ListMessagesResult,
  type ListSessionsParams,
  type ListSessionsResult,
  type SessionCheckpointsResult,
  type SessionFilesResult,
  type SessionSubagentsResult,
  type ListUsersParams,
  type ListUsersResult,
  type ListAuditResult,
  type AuditEntry,
  type AuditParams,
  type UsageModelRow,
  type UsageParams,
  type UsageReport,
  type UsageToolRow,
  type UsageUserRow,
  type ListMediaParams,
  type ListMediaResult,
  type MediaAsset,
  type LoginRequest,
  type MarketItem,
  type McpProbeResult,
  type McpServer,
  type McpServerInput,
  type McpTool,
  type AddCatalogModelRequest,
  type AddModelRouteRequest,
  type AddProviderKeyRequest,
  type AdminModelProvider,
  type AdminModelsResult,
  type CatalogModel,
  type HiddenModelsResult,
  type Memory,
  type MemorySlot,
  type MessageContent,
  type ModelCatalog,
  type ModelProvider,
  type ModelProviderKey,
  type ModelRoute,
  type Plan,
  type TenantModelsResult,
  type UpdateCatalogModelRequest,
  type UpdateModelRouteRequest,
  type UpdateProviderKeyRequest,
  type Plugin,
  type ProviderProbeResult,
  type ResetUserPasswordResult,
  type Run,
  type RunDetailResult,
  type SendMessageRequest,
  type SendMessageResult,
  type SessionDetailResult,
  type SessionEvent,
  type SessionEventType,
  type Skill,
  type UninstallMarketItemResult,
  type UpdateMemoryRequest,
  type UpdatePluginRequest,
  type UpdateProfileRequest,
  type UpdateSessionRequest,
  type UpdateSessionResult,
  type UpdateSkillRequest,
  type User,
} from './types'

// ── 配置 ────────────────────────────────────────────────────────────────

export type MockApprovalMode = 'auto' | 'always' | 'never'

export interface MockConfig {
  /** `auto`（默认）：内容提到审批/删除/部署时触发；`always` / `never` 强制开关。 */
  approvalMode: MockApprovalMode
  /** 每个 delta 的间隔区间（毫秒）。 */
  deltaDelayMs: [number, number]
}

const config: MockConfig = {
  approvalMode: 'auto',
  deltaDelayMs: [20, 40],
}

/** 仅用于本地演示/联调；生产页面不会调用。 */
export function configureMock(patch: Partial<MockConfig>): void {
  Object.assign(config, patch)
}

// ── 小工具 ──────────────────────────────────────────────────────────────

let idCounter = 0

function uid(prefix: string): string {
  idCounter += 1
  const rand = Math.random().toString(36).slice(2, 10)
  return `${prefix}_${rand}${idCounter.toString(36)}`
}

function nowIso(offsetMs = 0): string {
  return new Date(Date.now() + offsetMs).toISOString()
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function latency(): Promise<void> {
  return sleep(120 + Math.random() * 180)
}

function deltaDelay(): Promise<void> {
  const [min, max] = config.deltaDelayMs
  return sleep(min + Math.random() * Math.max(1, max - min))
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

// ── 内存库 ──────────────────────────────────────────────────────────────

interface MockState {
  users: User[]
  plans: Record<string, Plan | null>
  sessions: ChatSession[]
  messages: Record<string, ChatMessage[]>
  runs: Record<string, Run>
  currentUserId: string | null
  memories: Memory[]
  mcpServers: McpServer[]
  plugins: Plugin[]
  skills: Skill[]
  marketItems: MarketItem[]
  /** 租户模型服务：provider 开关 / 密钥池 / 目录 / 路由。 */
  modelProviders: AdminModelProvider[]
  modelKeys: ModelProviderKey[]
  catalogModels: CatalogModel[]
  modelRoutes: ModelRoute[]
  /** 用户级「在选择器中隐藏」列表，按用户 id 存。 */
  hiddenModels: Record<string, string[]>
}

const DEMO_USER_ID = 'usr_demo'
const DEMO_EMAIL = 'demo@example.com'

const DEMO_PLAN: Plan = {
  plan_id: 'team',
  max_users: 20,
  max_concurrent_runs: 4,
  monthly_tokens: 20_000_000,
  max_storage_gb: 50,
  max_sandboxes: 8,
}

const state: MockState = {
  users: [],
  plans: {},
  sessions: [],
  messages: {},
  runs: {},
  currentUserId: null,
  memories: [],
  mcpServers: [],
  plugins: [],
  skills: [],
  marketItems: [],
  modelProviders: [],
  modelKeys: [],
  catalogModels: [],
  modelRoutes: [],
  hiddenModels: {},
}

function demoUser(overrides: Partial<User> = {}): User {
  return {
    id: DEMO_USER_ID,
    email: DEMO_EMAIL,
    username: 'demo',
    name: '林清和',
    nickname: '清和',
    role: 'owner',
    status: 'active',
    must_change_password: false,
    password_set: true,
    last_login_at: nowIso(-86_400_000),
    ...overrides,
  }
}

function seedSession(
  id: string,
  title: string,
  status: 'active' | 'archived',
  model: string,
  updatedOffsetMs: number,
): ChatSession {
  return {
    id,
    tenant_id: 1,
    user_id: DEMO_USER_ID,
    parent_session_id: null,
    title,
    project_id: 'proj_internal',
    model,
    status,
    created_at: nowIso(updatedOffsetMs - 3_600_000),
    updated_at: nowIso(updatedOffsetMs),
    archived_at: status === 'archived' ? nowIso(updatedOffsetMs) : null,
    deleted_at: null,
  }
}

function seedMessage(
  sessionId: string,
  role: 'user' | 'assistant',
  content: MessageContent,
  createdAt: string,
  extra: Partial<ChatMessage> = {},
): ChatMessage {
  return {
    id: uid('msg'),
    session_id: sessionId,
    turn_id: uid('turn'),
    run_id: null,
    role,
    kind: 'chat',
    content,
    created_at: createdAt,
    ...extra,
  }
}

// ── 扩展功能的种子（记忆 / MCP / 插件 / 技能 / 市场） ───────────────────

const TENANT_ID = 1

/** 探测一定返回的 3 个假工具（stdio / http 两台 server 共用）。 */
const MOCK_PROBE_TOOLS: McpTool[] = [
  {
    name: 'read_file',
    description: '读取工作区内的文本文件',
    input_schema: {
      type: 'object',
      properties: { path: { type: 'string', description: '相对工作区路径' } },
      required: ['path'],
    },
  },
  {
    name: 'list_directory',
    description: '列出目录条目',
    input_schema: {
      type: 'object',
      properties: { path: { type: 'string' } },
    },
  },
  {
    name: 'search_docs',
    description: '在工作区文档里做关键字检索',
    input_schema: {
      type: 'object',
      properties: { query: { type: 'string' }, limit: { type: 'number' } },
      required: ['query'],
    },
  },
]

function seedMemory(
  id: string,
  slot: MemorySlot,
  scope: string,
  content: string,
  source: string,
  confidence: number,
  updatedOffsetMs: number,
): Memory {
  return {
    id,
    tenant_id: TENANT_ID,
    user_id: DEMO_USER_ID,
    scope,
    slot,
    content,
    source,
    confidence,
    session_id: null,
    created_at: nowIso(updatedOffsetMs - 86_400_000),
    updated_at: nowIso(updatedOffsetMs),
  }
}

function seedMcpServer(id: string, name: string, overrides: Partial<McpServer> = {}): McpServer {
  return {
    id,
    tenant_id: TENANT_ID,
    owner_user_id: DEMO_USER_ID,
    name,
    transport: 'stdio',
    command: 'npx',
    args: [],
    env: {},
    url: '',
    headers: {},
    tool_allowlist: [],
    enabled: true,
    protocol_version: null,
    server_info: null,
    tools: [],
    last_probe_at: null,
    last_error: '',
    created_at: nowIso(-10 * 86_400_000),
    updated_at: nowIso(-86_400_000),
    ...overrides,
  }
}

function seedPlugin(id: string, name: string, overrides: Partial<Plugin> = {}): Plugin {
  return {
    id,
    tenant_id: TENANT_ID,
    owner_user_id: DEMO_USER_ID,
    name,
    kind: 'mcp',
    version: '1.0.0',
    source: 'local',
    builtin: false,
    mcp_server_id: null,
    manifest: {
      name,
      version: '1.0.0',
      kind: 'mcp',
      description: '',
      mcp: { transport: 'stdio', command: 'npx', args: [] },
      tools: [],
      permissions: [],
    },
    enabled: true,
    created_at: nowIso(-7 * 86_400_000),
    updated_at: nowIso(-86_400_000),
    ...overrides,
  }
}

function seedSkill(id: string, name: string, overrides: Partial<Skill> = {}): Skill {
  return {
    id,
    tenant_id: TENANT_ID,
    owner_user_id: DEMO_USER_ID,
    name,
    version: '1.0.0',
    description: '',
    manifest: { name, version: '1.0.0', description: '', prompt: '', resources: [], scripts: [] },
    prompt: '',
    resources: [],
    scripts: [],
    source: 'local',
    enabled: true,
    created_at: nowIso(-7 * 86_400_000),
    updated_at: nowIso(-86_400_000),
    ...overrides,
  }
}

function seedMarketItem(
  id: string,
  itemId: string,
  name: string,
  kind: 'plugin' | 'skill',
  version: string,
  description: string,
  signed: boolean,
  publishedOffsetMs: number,
): MarketItem {
  return {
    id,
    item_id: itemId,
    name,
    kind,
    version,
    description,
    source: 'official',
    source_url: `https://market.example.com/${itemId}`,
    checksum: (signed ? 'a' : 'b').repeat(64),
    signed,
    published_at: nowIso(publishedOffsetMs),
  }
}

function seed(): void {
  // 成员列表直接复用 `users`：owner / admin / 普通成员 / 已停用各一。
  state.users = [
    demoUser(),
    demoUser({
      id: 'usr_admin',
      email: 'admin@example.com',
      username: 'admin',
      name: '周予安',
      nickname: '予安',
      role: 'admin',
      last_login_at: nowIso(-7_200_000),
    }),
    demoUser({
      id: 'usr_member',
      email: 'member@example.com',
      username: 'member',
      name: '苏晚',
      nickname: '晚晚',
      role: 'user',
      last_login_at: nowIso(-3 * 86_400_000),
    }),
    demoUser({
      id: 'usr_disabled',
      email: 'disabled@example.com',
      username: 'disabled',
      name: '停用账号',
      nickname: '',
      role: 'user',
      status: 'disabled',
      last_login_at: null,
    }),
  ]
  state.plans = {
    [DEMO_USER_ID]: DEMO_PLAN,
    usr_admin: DEMO_PLAN,
    usr_member: DEMO_PLAN,
    usr_disabled: DEMO_PLAN,
  }
  state.currentUserId = null
  state.runs = {}
  state.memories = []
  state.mcpServers = []
  state.plugins = []
  state.skills = []
  state.marketItems = []

  const s1 = 'ses_release_planner'
  const s2 = 'ses_log_triage'
  const s3 = 'ses_archived_pricing'

  state.sessions = [
    seedSession(s1, '发布计划梳理', 'active', 'openai:gpt-5.2', -120_000),
    seedSession(s2, '线上日志排障', 'active', 'anthropic:claude-sonnet-4-6', -3_600_000),
    seedSession(s3, '定价策略讨论（已归档）', 'archived', 'openai:gpt-5.2', -3 * 86_400_000),
  ]

  state.messages = {
    [s1]: [
      seedMessage(s1, 'user', '帮我把本周的发布计划整理一下，重点是对外可见的变更。', nowIso(-180_000)),
      seedMessage(
        s1,
        'assistant',
        {
          text: '好的，我按「对外可见」这个口径梳理了一遍本周变更：\n\n1. **会话事件流**：新增 `run.gap` 补偿，断线重连后自动重拉消息；\n2. **计费口径**：`usage.updated` 现在带 `cost` 字段；\n3. **配额预检**：并发 run 与月度 token 双闸门。\n\n需要我把第 1 项的验收步骤写成清单吗？',
        },
        nowIso(-150_000),
        { run_id: 'run_seed_1' },
      ),
      seedMessage(s1, 'user', '好，把验收步骤写出来，顺便列一下需要回滚的接口。', nowIso(-60_000)),
      seedMessage(
        s1,
        'assistant',
        {
          text: '可以，验收清单如下：\n\n- [ ] `GET /api/v1/sessions/{id}/events` 断线 3 次后仍能补齐事件；\n- [ ] 收到 `run.gap` 后消息列表与事件流一致；\n- [ ] 归档会话不可再发送消息（409）。\n\n回滚面只涉及 `sessions` 与 `events` 两个路由组。',
          tool_calls: [
            {
              id: 'call_seed_1',
              tool_call_id: 'call_seed_1',
              name: 'read_file',
              args: { path: 'docs/API.md' },
              status: 'completed',
            },
          ],
        },
        nowIso(-30_000),
        { run_id: 'run_seed_2' },
      ),
    ],
    [s2]: [
      seedMessage(s2, 'user', '昨晚 22:10 之后登录接口 5xx 变多，帮我看看可能原因。', nowIso(-3_600_000)),
      seedMessage(
        s2,
        'assistant',
        '从时间窗口看，22:10 恰好有一次租户连接池重建。建议先确认两件事：\n\n1. 连接池上限是否被并发 run 打满；\n2. `JWT_SECRET` 轮换后旧 token 是否集中在同一分钟过期。',
        nowIso(-3_560_000),
        { run_id: 'run_seed_3' },
      ),
    ],
    [s3]: [
      seedMessage(s3, 'user', '整理一下三档套餐的差异。', nowIso(-3 * 86_400_000)),
      seedMessage(
        s3,
        'assistant',
        '三档套餐主要差异在并发 run、月度 token 与沙箱数量，存储按 GB 线性递增。',
        nowIso(-3 * 86_400_000 + 60_000),
        { run_id: 'run_seed_4' },
      ),
    ],
  }

  // 记忆：persona / preferences / facts 各一条（scope 与后端一致用 `global`）。
  state.memories = [
    seedMemory(
      'mem_persona',
      'persona',
      'global',
      '资深 Rust 后端工程师，偏好显式错误处理、最小依赖与可回滚的小步提交。',
      'user',
      0.95,
      -3 * 86_400_000,
    ),
    seedMemory(
      'mem_preferences',
      'preferences',
      'global',
      '回答用中文；先给结论再展开；代码示例尽量可运行。',
      'user',
      0.9,
      -86_400_000,
    ),
    seedMemory('mem_facts', 'facts', 'global', 'project: chiron', 'auto', 0.8, -7_200_000),
  ]

  // MCP：stdio 正常、http 带 last_error。
  state.mcpServers = [
    seedMcpServer('mcp_fs_tools', 'fs-tools', {
      command: 'npx',
      args: ['-y', '@mcp/fs'],
      protocol_version: '2025-06-18',
      server_info: { name: 'fs-tools', version: '1.2.0' },
      tools: clone(MOCK_PROBE_TOOLS),
      last_probe_at: nowIso(-1_800_000),
    }),
    seedMcpServer('mcp_search_api', 'search-api', {
      transport: 'http',
      command: '',
      url: 'https://mcp.example.com/search',
      headers: { Authorization: 'Bearer demo-token' },
      server_info: null,
      tools: [],
      last_probe_at: nowIso(-3_600_000),
      last_error: 'connect timeout after 5000ms',
    }),
  ]

  // 插件：一个内置且启用，一个用户安装但已禁用。
  state.plugins = [
    seedPlugin('plg_fs_tools', 'fs-tools', {
      version: '1.2.0',
      source: 'builtin',
      builtin: true,
      mcp_server_id: 'mcp_fs_tools',
      enabled: true,
      manifest: {
        name: 'fs-tools',
        version: '1.2.0',
        kind: 'mcp',
        description: '内置文件系统工具（只读工作区）',
        mcp: { transport: 'stdio', command: 'npx', args: ['-y', '@mcp/fs'] },
        tools: ['read_file', 'list_directory'],
        permissions: ['fs:read'],
      },
    }),
    seedPlugin('plg_search_api', 'search-api', {
      version: '0.4.1',
      mcp_server_id: 'mcp_search_api',
      enabled: false,
      manifest: {
        name: 'search-api',
        version: '0.4.1',
        kind: 'mcp',
        description: '外部搜索 API 的 MCP 连接器（暂停使用）',
        mcp: { transport: 'http', url: 'https://mcp.example.com/search' },
        tools: [],
        permissions: [],
      },
    }),
  ]

  // 技能：一个启用的发布说明技能、一个禁用的日志排障技能。
  state.skills = [
    seedSkill('skl_release_notes', 'release-notes', {
      version: '1.0.0',
      description: '把本周变更整理成对外发布说明',
      prompt: '你是发布说明撰写者：按「对外可见」口径梳理变更，输出 Markdown 清单。',
      resources: [{ path: 'templates/release.md', description: '发布说明模板' }],
      scripts: [],
      manifest: {
        name: 'release-notes',
        version: '1.0.0',
        description: '把本周变更整理成对外发布说明',
        prompt: '你是发布说明撰写者：按「对外可见」口径梳理变更，输出 Markdown 清单。',
        resources: [{ path: 'templates/release.md', description: '发布说明模板' }],
        scripts: [],
      },
    }),
    seedSkill('skl_log_triage', 'log-triage', {
      version: '0.2.0',
      description: '按时间窗口归因线上错误日志',
      prompt: '先按时间窗口切分日志，再对齐部署/迁移事件，最后给出最小排查清单。',
      resources: [],
      scripts: [
        {
          name: 'grep_logs',
          path: 'scripts/grep_logs.sh',
          language: 'bash',
          description: '按关键字过滤日志',
          args: ['pattern'],
        },
      ],
      enabled: false,
      manifest: {
        name: 'log-triage',
        version: '0.2.0',
        description: '按时间窗口归因线上错误日志',
        prompt: '先按时间窗口切分日志，再对齐部署/迁移事件，最后给出最小排查清单。',
        resources: [],
        scripts: [
          {
            name: 'grep_logs',
            path: 'scripts/grep_logs.sh',
            language: 'bash',
            description: '按关键字过滤日志',
            args: ['pattern'],
          },
        ],
      },
    }),
  ]

  // 市场：两条插件、一条技能；签名一真一假。
  state.marketItems = [
    seedMarketItem(
      'mkt_fs_tools',
      'official/fs-tools',
      'Filesystem Tools',
      'plugin',
      '1.4.0',
      '读写工作区文件的 MCP 插件（含目录遍历）',
      true,
      -2 * 86_400_000,
    ),
    seedMarketItem(
      'mkt_search_api',
      'official/search-api',
      'Web Search Connector',
      'plugin',
      '0.9.2',
      '接入外部搜索服务的 MCP 插件',
      false,
      -5 * 86_400_000,
    ),
    seedMarketItem(
      'mkt_release_notes',
      'official/release-notes',
      'Release Notes Writer',
      'skill',
      '1.1.0',
      '按模板生成对外发布说明的技能',
      true,
      -9 * 86_400_000,
    ),
  ]

  seedModelService()
}

seed()

function currentUser(): User {
  const user = state.users.find((u) => u.id === state.currentUserId)
  if (!user) throw new ApiError(401, 'unauthorized', '未登录或登录已过期')
  return user
}

function authPayload(user: User): AuthPayload {
  return {
    token: `mock_${user.id}_${Date.now().toString(36)}`,
    user: clone(user),
    plan: state.plans[user.id] ?? null,
  }
}

function findSession(id: string): ChatSession {
  const session = state.sessions.find((s) => s.id === id && s.status !== 'deleted')
  if (!session) throw new ApiError(404, 'not_found', '会话不存在')
  return session
}

function normalizeIdentifier(identifier: string): string {
  const value = identifier.trim()
  if (!value) throw new ApiError(400, 'bad_request', '请输入邮箱或用户名')
  return value.includes('@') ? value.toLowerCase() : `${value.toLowerCase()}@example.com`
}

// ── 认证 ────────────────────────────────────────────────────────────────

export async function login(body: LoginRequest): Promise<AuthPayload> {
  await latency()
  const password = body.password ?? ''
  if (!password.trim()) throw new ApiError(400, 'bad_request', '请输入密码')

  const email = normalizeIdentifier(body.identifier)
  let user = state.users.find((u) => u.email === email)
  if (!user) {
    const mustChange = /reset|new/i.test(body.identifier)
    user = demoUser({
      id: uid('usr'),
      email,
      username: null,
      name: email.split('@')[0],
      nickname: '',
      role: 'member',
      must_change_password: mustChange,
      password_set: !mustChange,
      last_login_at: null,
    })
    state.users.push(user)
    state.plans[user.id] = DEMO_PLAN
  }
  user.last_login_at = nowIso()
  state.currentUserId = user.id
  return authPayload(user)
}

export async function logout(): Promise<void> {
  await latency()
  state.currentUserId = null
}

export async function session(): Promise<AuthPayload> {
  await sleep(160)
  return authPayload(currentUser())
}

export async function changePassword(oldPassword: string, newPassword: string): Promise<ChangePasswordResult> {
  await latency()
  if (!oldPassword.trim()) throw new ApiError(400, 'bad_request', '请输入当前密码')
  if (newPassword.length < 8) throw new ApiError(400, 'weak_password', '新密码至少 8 位')
  if (oldPassword === newPassword) throw new ApiError(400, 'bad_request', '新密码不能与当前密码相同')
  const user = currentUser()
  user.must_change_password = false
  user.password_set = true
  return { token: `mock_${user.id}_${Date.now().toString(36)}`, user: clone(user) }
}

export async function profile(): Promise<User> {
  await latency()
  return clone(currentUser())
}

export async function updateProfile(body: UpdateProfileRequest): Promise<User> {
  await latency()
  const user = currentUser()
  if (body.name !== undefined) {
    if (!body.name.trim()) throw new ApiError(400, 'bad_request', '显示名不能为空')
    user.name = body.name.trim()
  }
  if (body.nickname !== undefined) {
    user.nickname = body.nickname.trim()
  }
  return clone(user)
}

export async function listModels(): Promise<ModelCatalog> {
  await latency()
  return { providers: providerPresets() }
}

// ── 租户模型服务（管理端 + 用户目录） ──────────────────────────────────

/** provider preset 清单：`GET /models` 与模型管理页的下拉共用。 */
function providerPresets(): ModelProvider[] {
  return [
    {
      provider: 'openai',
      label: 'OpenAI',
      protocol: 'openai',
      base_url: 'https://api.openai.com/v1',
      model_prefixes: ['openai:', 'gpt-', 'o3'],
      requires_key: true,
      model_discovery: true,
    },
    {
      provider: 'anthropic',
      label: 'Anthropic',
      protocol: 'anthropic',
      base_url: 'https://api.anthropic.com',
      model_prefixes: ['anthropic:', 'claude-'],
      requires_key: true,
      model_discovery: true,
    },
    {
      provider: 'deepseek',
      label: 'DeepSeek',
      protocol: 'openai',
      base_url: 'https://api.deepseek.com/v1',
      model_prefixes: ['deepseek:'],
      requires_key: true,
      model_discovery: false,
    },
  ]
}

/** 模型服务的写操作延迟：120–250ms（与后端契约一致）。 */
function modelLatency(): Promise<void> {
  return sleep(120 + Math.random() * 130)
}

/** upstream 兜底：把 `provider:` 前缀去掉。 */
function stripProviderPrefix(model: string): string {
  const index = model.indexOf(':')
  return index > 0 ? model.slice(index + 1) : model
}

function providerLabelOf(provider: string): string {
  return providerPresets().find((item) => item.provider === provider)?.label ?? provider
}

/** 把任意输入收敛成 [min, max] 内的整数；非法值用 fallback。 */
function clampInt(value: unknown, min: number, max: number, fallback: number): number {
  const parsed = typeof value === 'number' ? value : Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(max, Math.max(min, Math.round(parsed)))
}

function seedModelService(): void {
  // 2 个 provider：deepseek（2 个启用 key）、openai（1 个 key，初始禁用）。
  state.modelProviders = [
    { provider: 'deepseek', label: 'DeepSeek', keys_count: 0, enabled: true },
    { provider: 'openai', label: 'OpenAI', keys_count: 0, enabled: true },
  ]
  state.modelKeys = [
    {
      id: 'key_ds_primary',
      provider: 'deepseek',
      label: '主力 Key',
      weight: 100,
      enabled: true,
      updated_at: nowIso(-3 * 86_400_000),
    },
    {
      id: 'key_ds_backup',
      provider: 'deepseek',
      label: '备用 Key',
      weight: 50,
      enabled: true,
      updated_at: nowIso(-9 * 86_400_000),
    },
    {
      id: 'key_openai_default',
      provider: 'openai',
      label: 'OpenAI 默认',
      weight: 100,
      enabled: false,
      updated_at: nowIso(-14 * 86_400_000),
    },
  ]
  state.catalogModels = [
    {
      model: 'deepseek:deepseek-chat',
      display_name: 'DeepSeek Chat',
      enabled: true,
      created_at: nowIso(-20 * 86_400_000),
      source: 'manual',
      discovered_at: null,
    },
    {
      model: 'openai:gpt-5-mini',
      display_name: 'GPT-5 mini',
      enabled: true,
      created_at: nowIso(-12 * 86_400_000),
      source: 'manual',
      discovered_at: null,
    },
  ]
  // deepseek 模型两条路由：deepseek 主用 + openai 备用。
  // 主用路由带一次成功的健康记录；备用路由从未探测，便于展示三种健康状态。
  state.modelRoutes = [
    {
      id: 'rt_ds_chat_primary',
      model: 'deepseek:deepseek-chat',
      provider: 'deepseek',
      upstream_model: 'deepseek-chat',
      priority: 1,
      weight: 100,
      enabled: true,
      last_ok_at: nowIso(-2 * 3_600_000),
      last_probe_at: nowIso(-2 * 3_600_000),
    },
    {
      id: 'rt_ds_chat_backup',
      model: 'deepseek:deepseek-chat',
      provider: 'openai',
      upstream_model: 'gpt-5-mini',
      priority: 2,
      weight: 50,
      enabled: true,
      last_ok_at: null,
      last_probe_at: null,
    },
  ]
  state.hiddenModels = {}
}

function findProviderKey(id: string): ModelProviderKey {
  const key = state.modelKeys.find((item) => item.id === id)
  if (!key) throw new ApiError(404, 'not_found', '密钥不存在')
  return key
}

function findModelRoute(id: string): ModelRoute {
  const route = state.modelRoutes.find((item) => item.id === id)
  if (!route) throw new ApiError(404, 'not_found', '路由不存在')
  return route
}

function providerWithCount(provider: AdminModelProvider): AdminModelProvider {
  return {
    ...provider,
    keys_count: state.modelKeys.filter((key) => key.provider === provider.provider).length,
  }
}

/** 管理端总览：provider 开关、密钥池、目录与路由。 */
export async function adminModels(): Promise<AdminModelsResult> {
  await modelLatency()
  return clone({
    providers: state.modelProviders.map(providerWithCount),
    keys: state.modelKeys,
    models: state.catalogModels,
    routes: state.modelRoutes,
  })
}

export async function addProviderKey(body: AddProviderKeyRequest): Promise<ModelProviderKey> {
  await modelLatency()
  const provider = body.provider?.trim() ?? ''
  if (!provider) throw new ApiError(400, 'bad_request', '请选择 Provider')
  const apiKey = body.api_key?.trim() ?? ''
  if (!apiKey) throw new ApiError(400, 'bad_request', '请输入 API Key')

  // 未登记过的 provider 自动补一条（添加密钥即视为启用该 provider）。
  if (!state.modelProviders.some((item) => item.provider === provider)) {
    state.modelProviders.push({
      provider,
      label: providerLabelOf(provider),
      keys_count: 0,
      enabled: true,
    })
  }
  const key: ModelProviderKey = {
    id: uid('key'),
    provider,
    label: body.label?.trim() || `${providerLabelOf(provider)} Key`,
    weight: clampInt(body.weight, 1, 100, 100),
    enabled: true,
    updated_at: nowIso(),
  }
  state.modelKeys.push(key)
  return clone(key)
}

export async function updateProviderKey(
  id: string,
  body: UpdateProviderKeyRequest,
): Promise<ModelProviderKey> {
  await modelLatency()
  const key = findProviderKey(id)
  if (body.enabled !== undefined) key.enabled = Boolean(body.enabled)
  if (body.weight !== undefined) key.weight = clampInt(body.weight, 1, 100, key.weight)
  key.updated_at = nowIso()
  return clone(key)
}

export async function deleteProviderKey(id: string): Promise<void> {
  await modelLatency()
  findProviderKey(id)
  state.modelKeys = state.modelKeys.filter((item) => item.id !== id)
}

export async function setProviderEnabled(
  provider: string,
  enabled: boolean,
): Promise<AdminModelProvider> {
  await modelLatency()
  const row = state.modelProviders.find((item) => item.provider === provider)
  if (!row) throw new ApiError(404, 'not_found', 'Provider 不存在')
  row.enabled = Boolean(enabled)
  return clone(providerWithCount(row))
}

export async function addCatalogModel(body: AddCatalogModelRequest): Promise<CatalogModel> {
  await modelLatency()
  const model = body.model?.trim() ?? ''
  if (!model) throw new ApiError(400, 'bad_request', '请输入模型 ID')
  if (state.catalogModels.some((item) => item.model === model)) {
    throw new ApiError(409, 'conflict', '模型已存在')
  }
  const entry: CatalogModel = {
    model,
    display_name: body.display_name?.trim() || model,
    enabled: true,
    created_at: nowIso(),
    source: 'manual',
    discovered_at: null,
  }
  state.catalogModels.push(entry)
  return clone(entry)
}

export async function updateCatalogModel(
  model: string,
  body: UpdateCatalogModelRequest,
): Promise<CatalogModel> {
  await modelLatency()
  const entry = state.catalogModels.find((item) => item.model === model)
  if (!entry) throw new ApiError(404, 'not_found', '模型不存在')
  if (body.enabled !== undefined) entry.enabled = Boolean(body.enabled)
  if (body.display_name !== undefined) {
    entry.display_name = body.display_name.trim() || entry.model
  }
  return clone(entry)
}

export async function deleteCatalogModel(model: string): Promise<void> {
  await modelLatency()
  const entry = state.catalogModels.find((item) => item.model === model)
  if (!entry) throw new ApiError(404, 'not_found', '模型不存在')
  state.catalogModels = state.catalogModels.filter((item) => item.model !== model)
  // 目录模型删除后其路由与用户隐藏项一并清理，避免悬挂引用。
  state.modelRoutes = state.modelRoutes.filter((route) => route.model !== model)
  for (const userId of Object.keys(state.hiddenModels)) {
    state.hiddenModels[userId] = state.hiddenModels[userId].filter((item) => item !== model)
  }
}

export async function addModelRoute(body: AddModelRouteRequest): Promise<ModelRoute> {
  await modelLatency()
  const model = body.model?.trim() ?? ''
  if (!model) throw new ApiError(400, 'bad_request', '请选择模型')
  if (!state.catalogModels.some((item) => item.model === model)) {
    throw new ApiError(400, 'bad_request', '模型不在目录中')
  }
  const provider = body.provider?.trim() ?? ''
  if (!provider) throw new ApiError(400, 'bad_request', '请输入 Provider')
  if (state.modelRoutes.some((item) => item.model === model && item.provider === provider)) {
    throw new ApiError(409, 'conflict', '该模型已存在相同 Provider 的路由')
  }
  const route: ModelRoute = {
    id: uid('rt'),
    model,
    provider,
    upstream_model: body.upstream_model?.trim() || stripProviderPrefix(model),
    priority: clampInt(body.priority, 1, 1000, 100),
    weight: clampInt(body.weight, 1, 100, 100),
    enabled: true,
    last_ok_at: null,
    last_probe_at: null,
  }
  state.modelRoutes.push(route)
  return clone(route)
}

export async function updateModelRoute(
  id: string,
  body: UpdateModelRouteRequest,
): Promise<ModelRoute> {
  await modelLatency()
  const route = findModelRoute(id)
  if (body.enabled !== undefined) route.enabled = Boolean(body.enabled)
  if (body.upstream_model !== undefined) {
    route.upstream_model = body.upstream_model.trim() || stripProviderPrefix(route.model)
  }
  if (body.priority !== undefined) route.priority = clampInt(body.priority, 1, 1000, route.priority)
  if (body.weight !== undefined) route.weight = clampInt(body.weight, 1, 100, route.weight)
  return clone(route)
}

export async function deleteModelRoute(id: string): Promise<void> {
  await modelLatency()
  findModelRoute(id)
  state.modelRoutes = state.modelRoutes.filter((item) => item.id !== id)
}

/** mock 的「远端模型清单」：只给 deepseek 返回结果，其他 provider 视为无模型。 */
const DISCOVERABLE_MODELS: Record<string, string[]> = {
  deepseek: ['deepseek-chat', 'deepseek-reasoner'],
}

/** provider 是否有至少一把启用的密钥（没有就模拟「missing API key」）。 */
function hasUsableKey(provider: string): boolean {
  return state.modelKeys.some((key) => key.provider === provider && key.enabled)
}

/** 目录 ID：upstream 名补上 `provider:` 前缀。 */
function catalogIdOf(provider: string, upstream: string): string {
  const value = upstream.trim()
  return value.includes(':') ? value : `${provider}:${value}`
}

/** 单 provider 的发现流程：没有可用密钥直接失败；persist 时落目录 + 生成路由。 */
function discoverOne(provider: string, persist: boolean): DiscoverProviderResult {
  const known =
    state.modelProviders.some((item) => item.provider === provider) ||
    providerPresets().some((item) => item.provider === provider)
  if (!known) return { provider, ok: false, models: [], error: 'unknown provider' }
  if (!hasUsableKey(provider)) {
    return { provider, ok: false, models: [], error: 'missing API key' }
  }

  const upstreams = DISCOVERABLE_MODELS[provider] ?? []
  const discoveredAt = nowIso()
  const models = upstreams.map((upstream) => {
    const id = catalogIdOf(provider, upstream)
    const exists = state.catalogModels.some((item) => item.model === id)
    if (persist && !exists) {
      state.catalogModels.push({
        model: id,
        display_name: upstream,
        enabled: true,
        created_at: discoveredAt,
        source: 'discovered',
        discovered_at: discoveredAt,
      })
      if (!state.modelRoutes.some((route) => route.model === id && route.provider === provider)) {
        state.modelRoutes.push({
          id: uid('rt'),
          model: id,
          provider,
          upstream_model: upstream,
          // 与后端默认一致：priority 100 / weight 1，管理员可再调。
          priority: 100,
          weight: 1,
          enabled: true,
          last_ok_at: null,
          last_probe_at: null,
        })
      }
    }
    return { model: upstream, exists }
  })

  return { provider, ok: true, models }
}

/** 自动发现：缺省遍历全部 provider；`persist=true` 时写入目录并生成路由。 */
export async function discoverModels(
  body: DiscoverModelsRequest = {},
): Promise<DiscoverModelsResult> {
  await modelLatency()
  const persist = Boolean(body.persist)
  const requested = body.provider?.trim()
  const targets = requested
    ? [requested]
    : state.modelProviders.map((item) => item.provider)
  return clone({ results: targets.map((provider) => discoverOne(provider, persist)) })
}

/** 健康探测：deepseek 成功并刷新路由健康，openai 因缺密钥失败。 */
export async function probeProvider(provider: string): Promise<ProviderProbeResult> {
  await modelLatency()
  if (!state.modelProviders.some((item) => item.provider === provider)) {
    throw new ApiError(404, 'not_found', 'Provider 不存在')
  }
  const probedAt = nowIso()
  const routes = state.modelRoutes.filter((route) => route.provider === provider)

  if (!hasUsableKey(provider)) {
    const error = 'missing API key'
    for (const route of routes) {
      route.last_error = error
      route.last_ok_at = null
      route.last_probe_at = probedAt
    }
    return { ok: false, error }
  }

  for (const route of routes) {
    route.last_ok_at = probedAt
    route.last_error = undefined
    route.last_probe_at = probedAt
  }
  const modelCount = state.catalogModels.filter((item) => item.model.startsWith(`${provider}:`))
    .length
  return { ok: true, models: modelCount }
}

/** 普通用户目录：只返回启用的模型，`providers` 为当前可用路由的 provider 列表。 */
export async function listTenantModels(): Promise<TenantModelsResult> {
  await modelLatency()
  const enabledProviders = new Set(
    state.modelProviders.filter((item) => item.enabled).map((item) => item.provider),
  )
  const models = state.catalogModels
    .filter((item) => item.enabled)
    .map((item) => {
      const routed = state.modelRoutes
        .filter(
          (route) =>
            route.model === item.model && route.enabled && enabledProviders.has(route.provider),
        )
        .map((route) => route.provider)
      const providers = Array.from(new Set(routed))
      if (providers.length === 0) {
        // 没有路由时用模型前缀兜底，卡片仍能显示来源 provider。
        const prefix = item.model.includes(':') ? item.model.slice(0, item.model.indexOf(':')) : ''
        if (prefix) providers.push(prefix)
      }
      return { model: item.model, display_name: item.display_name, providers }
    })
  return { models, providers: providerPresets() }
}

/** 用户隐藏列表：未登录时按匿名桶保存（页面本身要求登录）。 */
function hiddenList(): string[] {
  const key = state.currentUserId ?? 'anonymous'
  if (!state.hiddenModels[key]) state.hiddenModels[key] = []
  return state.hiddenModels[key]
}

export async function getHiddenModels(): Promise<HiddenModelsResult> {
  await modelLatency()
  return { models: [...hiddenList()] }
}

export async function setHiddenModels(models: string[]): Promise<HiddenModelsResult> {
  await modelLatency()
  const seen = new Set<string>()
  const next: string[] = []
  for (const item of Array.isArray(models) ? models : []) {
    if (typeof item !== 'string') continue
    const value = item.trim()
    if (!value || seen.has(value)) continue
    seen.add(value)
    next.push(value)
  }
  state.hiddenModels[state.currentUserId ?? 'anonymous'] = next
  return { models: [...next] }
}

// ── 会话 ────────────────────────────────────────────────────────────────

export async function listSessions(params: ListSessionsParams = {}): Promise<ListSessionsResult> {
  await latency()
  const status = params.status ?? 'active'
  const limit = Math.min(Math.max(params.limit ?? 30, 1), 100)
  const rows = state.sessions
    .filter((s) => s.status !== 'deleted')
    .filter((s) => status === 'all' || s.status === status)
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
  const start = params.cursor ? Math.max(0, rows.findIndex((s) => s.id === params.cursor) + 1) : 0
  const page = rows.slice(start, start + limit)
  const activeFor = (sessionId: string) =>
    Object.values(state.runs).find(
      (run) =>
        run.session_id === sessionId &&
        ['queued', 'running', 'waiting_approval', 'waiting_user'].includes(run.status),
    )
  return {
    sessions: page.map((session) => {
      const active = activeFor(session.id)
      return {
        ...clone(session),
        active_run: active ? { run_id: active.id, status: active.status } : null,
        pending_approvals: active?.status === 'waiting_approval' ? 1 : 0,
      }
    }),
    next_cursor: start + limit < rows.length && page.length > 0 ? page[page.length - 1].id : null,
    pending_approvals_total: state.sessions.filter(
      (session) => activeFor(session.id)?.status === 'waiting_approval',
    ).length,
  }
}

export async function createSession(body: CreateSessionRequest = {}): Promise<CreateSessionResult> {
  await latency()
  const user = currentUser()
  const now = nowIso()
  const session: ChatSession = {
    id: uid('ses'),
    tenant_id: 1,
    user_id: user.id,
    parent_session_id: body.parent_session_id ?? null,
    title: body.title?.trim() || '新会话',
    project_id: body.project_id ?? null,
    model: body.model ?? 'openai:gpt-5.2',
    status: 'active',
    created_at: now,
    updated_at: now,
    archived_at: null,
    deleted_at: null,
  }
  state.sessions.unshift(session)
  state.messages[session.id] = []
  return { session: clone(session) }
}

export async function getSession(id: string): Promise<SessionDetailResult> {
  await latency()
  const session = findSession(id)
  const active = Object.values(state.runs).find(
    (r) =>
      r.session_id === id &&
      ['queued', 'running', 'waiting_approval', 'waiting_user'].includes(r.status),
  )
  return {
    session: clone(session),
    active_run: active ? { run_id: active.id, status: active.status } : null,
  }
}

export async function updateSession(id: string, body: UpdateSessionRequest): Promise<UpdateSessionResult> {
  await latency()
  const session = findSession(id)
  if (body.title !== undefined) {
    const title = body.title.trim()
    if (!title) throw new ApiError(400, 'bad_request', '标题不能为空')
    session.title = title
  }
  if (body.model !== undefined) session.model = body.model.trim() || null
  const archived = body.archived ?? (body.status ? body.status === 'archived' : undefined)
  if (archived !== undefined) {
    session.status = archived ? 'archived' : 'active'
    session.archived_at = archived ? nowIso() : null
  }
  session.updated_at = nowIso()
  return { session: clone(session) }
}

export async function deleteSession(id: string): Promise<void> {
  await latency()
  const session = findSession(id)
  session.status = 'deleted'
  session.deleted_at = nowIso()
}

// ── 消息 ────────────────────────────────────────────────────────────────

export async function listMessages(
  sessionId: string,
  params: ListMessagesParams = {},
): Promise<ListMessagesResult> {
  await sleep(140)
  findSession(sessionId)
  const all = state.messages[sessionId] ?? []
  const limit = Math.min(Math.max(params.limit ?? 60, 1), 200)
  const start = params.cursor ? Math.max(0, all.findIndex((m) => m.id === params.cursor) + 1) : 0
  const page = all.slice(start, start + limit)
  return {
    messages: clone(page),
    next_cursor: start + limit < all.length && page.length > 0 ? page[page.length - 1].id : null,
  }
}

export async function listSessionCheckpoints(
  sessionId: string,
  _limit = 20,
): Promise<SessionCheckpointsResult> {
  await latency()
  findSession(sessionId)
  return { session_id: sessionId, checkpoints: [], count: 0 }
}

export async function restoreCheckpoint(id: string): Promise<Record<string, unknown>> {
  await latency()
  return { checkpoint_id: id, restored: true }
}

export async function listSessionSubagents(
  sessionId: string,
  _limit = 100,
): Promise<SessionSubagentsResult> {
  await latency()
  findSession(sessionId)
  return { session_id: sessionId, subagents: [], count: 0 }
}

export async function cancelSubagent(id: string): Promise<Record<string, unknown>> {
  await latency()
  return { subagent_id: id, status: 'cancelled' }
}

export async function listSessionFiles(
  sessionId: string,
  path = '',
  _limit = 200,
): Promise<SessionFilesResult> {
  await latency()
  findSession(sessionId)
  return { path, entries: [], truncated: false }
}

// ── 事件总线 ────────────────────────────────────────────────────────────

type MockListener = (event: SessionEvent) => void

const listeners = new Map<string, Set<MockListener>>()
const seqBySession = new Map<string, number>()

function nextSeq(sessionId: string): number {
  const seq = (seqBySession.get(sessionId) ?? 0) + 1
  seqBySession.set(sessionId, seq)
  return seq
}

function emit<K extends SessionEventType>(
  sessionId: string,
  type: K,
  data: EventDataMap[K],
  runId: string | null,
): void {
  const event = {
    id: uid('evt'),
    seq: nextSeq(sessionId),
    run_id: runId,
    type,
    data,
    ts: nowIso(),
  } as SessionEvent
  const set = listeners.get(sessionId)
  if (!set) return
  for (const listener of Array.from(set)) {
    try {
      listener(event)
    } catch {
      // 单个订阅者异常不影响事件流本身
    }
  }
}

/** 订阅 mock 事件流；返回取消函数。多次订阅互不影响。 */
export function subscribeMock(sessionId: string, onEvent: (event: SessionEvent) => void): () => void {
  let set = listeners.get(sessionId)
  if (!set) {
    set = new Set()
    listeners.set(sessionId, set)
  }
  set.add(onEvent)
  return () => {
    const current = listeners.get(sessionId)
    if (!current) return
    current.delete(onEvent)
    if (current.size === 0) listeners.delete(sessionId)
  }
}

// ── Run 执行（流式） ────────────────────────────────────────────────────

interface MockRunControl {
  cancelled: boolean
  approval?: { resolve: (decision: ApprovalDecision) => void }
  assistantMessageId: string
}

const runControls = new Map<string, MockRunControl>()

function shouldAskApproval(content: string): boolean {
  if (config.approvalMode === 'always') return true
  if (config.approvalMode === 'never') return false
  return /审批|approval|删除|deploy|部署|rm -rf/i.test(content)
}

function markdownReply(prompt: string): string {
  const short = prompt.length > 24 ? `${prompt.slice(0, 24)}…` : prompt
  return [
    '## 收到，我先给出结论',
    '',
    `针对「${short}」，我会分三步处理：`,
    '',
    '1. **确认范围**：只改动会话与事件相关代码，不动计费与配额。',
    '2. **最小改动**：先补 SSE 断线重连，再补 `run.gap` 后的消息重拉。',
    '3. **验收**：跑一遍单元测试，再手工验证三条主路径。',
    '',
    '下面是关键片段：',
    '',
    '```ts',
    'const unsubscribe = subscribeSessionEvents(sessionId, {',
    '  onEvent: (event) => apply(event),',
    '  onGap: () => reloadMessages(),',
    '})',
    '```',
    '',
    '> 提示：断线重连会用 `Last-Event-ID` 带上最后收到的 `seq`，服务端只补发之后的事件。',
    '',
    '需要我继续把变更同步到文档里吗？',
  ].join('\n')
}

function chunkContent(content: string): string[] {
  const chunks: string[] = []
  let cursor = 0
  while (cursor < content.length) {
    const size = 8 + Math.floor(Math.random() * 16)
    chunks.push(content.slice(cursor, cursor + size))
    cursor += size
  }
  return chunks
}

function makeRun(sessionId: string, runId: string): Run {
  const now = nowIso()
  return {
    id: runId,
    session_id: sessionId,
    turn_id: uid('turn'),
    status: 'queued',
    error: null,
    usage: {},
    created_at: now,
    updated_at: now,
    started_at: null,
    finished_at: null,
    lease_owner: 'mock',
    lease_until: null,
  }
}

function activeRunFor(sessionId: string): Run | undefined {
  return Object.values(state.runs).find(
    (r) =>
      r.session_id === sessionId &&
      ['queued', 'running', 'waiting_approval', 'waiting_user'].includes(r.status),
  )
}

/** 流式产出一条完整 run；返回时 run 已进入终态（或等待审批）。 */
async function streamRun(sessionId: string, runId: string, prompt: string, model: string): Promise<void> {
  const control = runControls.get(runId)
  const run = state.runs[runId]
  if (!control || !run) return

  const assistantId = control.assistantMessageId
  const content = markdownReply(prompt)
  const toolCallId = uid('call')

  try {
    run.status = 'running'
    run.started_at = nowIso()
    emit(sessionId, 'run.started', { model, turn_id: run.turn_id ?? undefined }, runId)

    for (const delta of chunkContent(content)) {
      if (control.cancelled) return
      await deltaDelay()
      if (control.cancelled) return
      emit(sessionId, 'message.delta', { message_id: assistantId, delta }, runId)
    }

    if (control.cancelled) return
    await sleep(120)
    if (control.cancelled) return

    emit(sessionId, 'message.completed', { message_id: assistantId, content }, runId)
    emit(
      sessionId,
      'tool.call',
      { tool_call_id: toolCallId, name: 'search_docs', args: { query: 'sse reconnect gap', limit: 3 } },
      runId,
    )

    await sleep(240)
    if (control.cancelled) return

    emit(
      sessionId,
      'tool.result',
      {
        tool_call_id: toolCallId,
        status: 'ok',
        result: { hits: 3, top: 'docs/API.md#会话事件流' },
        error: null,
      },
      runId,
    )

    if (shouldAskApproval(prompt)) {
      run.status = 'waiting_approval'
      const approvalId = uid('apr')
      const decision = await new Promise<ApprovalDecision>((resolve) => {
        control.approval = { resolve }
        emit(
          sessionId,
          'approval.required',
          {
            approval_id: approvalId,
            tool_call_id: toolCallId,
            name: 'run_tests',
            args: { command: 'npm test -- sessions' },
            expires_at: nowIso(120_000),
          },
          runId,
        )
      })
      emit(sessionId, 'approval.resolved', { approval_id: approvalId, decision }, runId)
      control.approval = undefined
      if (control.cancelled) return
      run.status = 'running'
      if (decision === 'deny') {
        emit(
          sessionId,
          'tool.result',
          { tool_call_id: toolCallId, status: 'denied', result: null, error: '用户拒绝了执行' },
          runId,
        )
      }
    }

    const usage = { input_tokens: 1820, output_tokens: 640, cost: 0.0123 }
    emit(sessionId, 'usage.updated', usage, runId)

    state.messages[sessionId] = state.messages[sessionId] ?? []
    state.messages[sessionId].push({
      id: assistantId,
      session_id: sessionId,
      turn_id: run.turn_id,
      run_id: runId,
      role: 'assistant',
      kind: 'chat',
      content: {
        text: content,
        tool_calls: [
          {
            id: toolCallId,
            tool_call_id: toolCallId,
            name: 'search_docs',
            args: { query: 'sse reconnect gap', limit: 3 },
            status: 'ok',
            result: { hits: 3 },
          },
        ],
      },
      created_at: nowIso(),
    })

    const session = state.sessions.find((s) => s.id === sessionId)
    if (session) session.updated_at = nowIso()

    run.status = 'completed'
    run.usage = usage
    run.finished_at = nowIso()
    run.updated_at = run.finished_at
    emit(sessionId, 'run.completed', { status: 'completed', usage }, runId)
  } finally {
    runControls.delete(runId)
  }
}

// ── Run 控制 ────────────────────────────────────────────────────────────

export async function sendMessage(
  sessionId: string,
  body: SendMessageRequest,
  options: { idempotencyKey?: string; model?: string } = {},
): Promise<SendMessageResult> {
  const session = findSession(sessionId)
  if (session.status !== 'active') {
    throw new ApiError(409, 'session_archived', '会话已归档，无法发送消息')
  }

  const text = typeof body.content === 'string' ? body.content : body.content.text
  if (!text.trim()) throw new ApiError(400, 'bad_request', '消息内容不能为空')

  const model = body.model ?? options.model ?? session.model ?? 'openai:gpt-5.2'
  const runId = uid('run')
  const run = makeRun(sessionId, runId)
  const userMessageId = uid('msg')
  const assistantId = uid('msg')

  state.messages[sessionId] = state.messages[sessionId] ?? []
  state.messages[sessionId].push({
    id: userMessageId,
    session_id: sessionId,
    turn_id: run.turn_id,
    run_id: runId,
    role: 'user',
    kind: 'chat',
    content: text,
    created_at: nowIso(),
  })
  session.updated_at = nowIso()

  state.runs[runId] = run
  runControls.set(runId, { cancelled: false, assistantMessageId: assistantId })

  void streamRun(sessionId, runId, text, model)

  return {
    session_id: sessionId,
    message_id: userMessageId,
    turn_id: run.turn_id ?? undefined,
    run_id: runId,
    status: 'queued',
    model,
  }
}

export async function interrupt(sessionId: string, reason?: string): Promise<{ cancelled: boolean }> {
  await sleep(120)
  findSession(sessionId)
  const active = activeRunFor(sessionId)
  if (!active) return { cancelled: false }
  const control = runControls.get(active.id)
  if (control) {
    control.cancelled = true
    control.approval?.resolve('deny')
  }
  active.status = 'cancelled'
  active.error = { reason: reason ?? 'user_interrupt' }
  active.finished_at = nowIso()
  active.updated_at = active.finished_at
  emit(sessionId, 'run.cancelled', { reason: reason ?? 'user_interrupt' }, active.id)
  return { cancelled: true }
}

export async function steer(sessionId: string, content: string): Promise<{ accepted: boolean }> {
  await sleep(120)
  findSession(sessionId)
  if (!activeRunFor(sessionId)) {
    throw new ApiError(409, 'no_active_run', '当前没有进行中的运行')
  }
  state.messages[sessionId] = state.messages[sessionId] ?? []
  state.messages[sessionId].push({
    id: uid('msg'),
    session_id: sessionId,
    turn_id: null,
    run_id: null,
    role: 'user',
    kind: 'steer',
    content,
    created_at: nowIso(),
  })
  return { accepted: true }
}

export async function resolveApproval(
  sessionId: string,
  approvalId: string,
  decision: ApprovalDecision,
  _note?: string,
): Promise<{ approval_id: string; decision: ApprovalDecision }> {
  await sleep(160)
  findSession(sessionId)
  const active = activeRunFor(sessionId)
  const control = active ? runControls.get(active.id) : undefined
  if (!control?.approval) {
    throw new ApiError(409, 'approval_not_pending', '审批已失效或不存在')
  }
  control.approval.resolve(decision)
  return { approval_id: approvalId, decision }
}

export async function getRun(runId: string): Promise<RunDetailResult> {
  await sleep(140)
  const run = state.runs[runId]
  if (!run) throw new ApiError(404, 'not_found', '运行记录不存在')
  return {
    run: clone(run),
    turn: run.turn_id
      ? { id: run.turn_id, status: run.status, model: null, usage: clone(run.usage) }
      : null,
  }
}

// ── 记忆 ────────────────────────────────────────────────────────────────

const GLOBAL_MEMORY_SCOPE = 'global'

function parseMemorySlot(raw: string): MemorySlot {
  const value = raw.trim().toLowerCase()
  if (value === 'persona' || value === 'preferences' || value === 'facts' || value === 'summary') {
    return value
  }
  throw new ApiError(400, 'bad_request', 'slot must be persona, preferences, facts or summary')
}

/**
 * 作用域归一化：后端口径是 `global` 或 `session:<uuid>`；页面把「用户级」
 * 写成 `user` 时一并归一为 `global`，避免同一语义出现两套取值。
 */
function normalizeMemoryScope(raw: string | null | undefined, sessionId?: string): string {
  const value = (raw ?? '').trim()
  if (!value) return sessionId ? `session:${sessionId}` : GLOBAL_MEMORY_SCOPE
  if (value === 'user' || value === GLOBAL_MEMORY_SCOPE) return GLOBAL_MEMORY_SCOPE
  if (value.startsWith('session:')) return value
  throw new ApiError(400, 'bad_request', 'scope must be `global` or `session:<uuid>`')
}

function normalizeMemorySource(raw?: string | null): string {
  const value = (raw ?? 'user').trim().toLowerCase()
  if (value === 'user' || value === 'auto' || value === 'import') return value
  throw new ApiError(400, 'bad_request', 'source must be user, auto or import')
}

function normalizeConfidence(raw?: number): number {
  if (raw === undefined) return 1
  if (!Number.isFinite(raw)) {
    throw new ApiError(400, 'bad_request', 'confidence must be a number between 0 and 1')
  }
  return Math.min(Math.max(raw, 0), 1)
}

export async function listMemories(params: ListMemoriesParams = {}): Promise<Memory[]> {
  await latency()
  const scope = params.scope?.trim() ? normalizeMemoryScope(params.scope) : null
  const slot = params.slot?.trim() ? parseMemorySlot(params.slot) : null
  const rows = state.memories
    .filter((m) => !scope || m.scope === scope)
    .filter((m) => !slot || m.slot === slot)
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
  return clone(rows)
}

export async function createMemory(body: CreateMemoryRequest): Promise<Memory> {
  await latency()
  const content = (body.content ?? '').trim()
  if (!content) throw new ApiError(400, 'bad_request', '记忆内容不能为空')
  const slot = parseMemorySlot(body.slot)
  const scope = normalizeMemoryScope(body.scope, body.session_id)
  if (slot === 'summary' && !scope.startsWith('session:')) {
    throw new ApiError(400, 'bad_request', 'summary memories require scope `session:<uuid>`')
  }
  const sessionId = scope.startsWith('session:') ? scope.slice('session:'.length) : null
  const source = normalizeMemorySource(body.source)
  const confidence = normalizeConfidence(body.confidence)
  const now = nowIso()

  // persona / summary 是单值槽位：同作用域只保留一条（与后端的覆盖语义一致）。
  if (slot === 'persona' || slot === 'summary') {
    const existing = state.memories.find((m) => m.scope === scope && m.slot === slot)
    if (existing) {
      existing.content = content
      existing.source = source
      existing.confidence = confidence
      existing.session_id = sessionId
      existing.updated_at = now
      return clone(existing)
    }
  }

  const memory: Memory = {
    id: uid('mem'),
    tenant_id: TENANT_ID,
    user_id: state.currentUserId ?? DEMO_USER_ID,
    scope,
    slot,
    content,
    source,
    confidence,
    session_id: sessionId,
    created_at: now,
    updated_at: now,
  }
  state.memories.unshift(memory)
  return clone(memory)
}

export async function updateMemory(id: string, body: UpdateMemoryRequest): Promise<Memory> {
  await latency()
  const memory = state.memories.find((m) => m.id === id)
  if (!memory) throw new ApiError(404, 'not_found', '记忆不存在')
  if (body.content !== undefined) {
    const content = body.content.trim()
    if (!content) throw new ApiError(400, 'bad_request', '记忆内容不能为空')
    memory.content = content
  }
  if (body.slot !== undefined) memory.slot = parseMemorySlot(body.slot)
  if (body.source !== undefined) memory.source = normalizeMemorySource(body.source)
  if (body.confidence !== undefined) memory.confidence = normalizeConfidence(body.confidence)
  memory.updated_at = nowIso()
  return clone(memory)
}

export async function deleteMemory(id: string): Promise<void> {
  await latency()
  const index = state.memories.findIndex((m) => m.id === id)
  if (index < 0) throw new ApiError(404, 'not_found', '记忆不存在')
  state.memories.splice(index, 1)
}

export async function clearMemories(
  params: { scope?: string; slot?: string } = {},
): Promise<ClearMemoriesResult> {
  await latency()
  const scope = params.scope?.trim() ? normalizeMemoryScope(params.scope) : null
  const slot = params.slot?.trim() ? parseMemorySlot(params.slot) : null
  const before = state.memories.length
  state.memories = state.memories.filter((m) => {
    if (scope && m.scope !== scope) return true
    if (slot && m.slot !== slot) return true
    return false
  })
  return { deleted: before - state.memories.length }
}

// ── MCP ─────────────────────────────────────────────────────────────────

function normalizeTransport(raw?: string): 'stdio' | 'sse' | 'http' {
  const value = (raw ?? 'stdio').trim().toLowerCase()
  if (value === 'stdio' || value === 'sse' || value === 'http') return value
  throw new ApiError(400, 'bad_request', 'transport must be stdio, sse or http')
}

function findMcpServer(id: string): McpServer {
  const server = state.mcpServers.find((s) => s.id === id)
  if (!server) throw new ApiError(404, 'not_found', 'MCP server 不存在')
  return server
}

export async function listMcpServers(): Promise<McpServer[]> {
  await latency()
  return clone(state.mcpServers)
}

export async function createMcpServer(body: McpServerInput): Promise<McpServer> {
  await latency()
  const name = body.name?.trim() ?? ''
  if (!name) throw new ApiError(400, 'bad_request', 'name is required')
  const transport = normalizeTransport(body.transport)
  if (state.mcpServers.some((s) => s.name === name)) {
    throw new ApiError(409, 'conflict', '同名 MCP server 已存在')
  }
  if (transport !== 'stdio' && !body.url?.trim()) {
    throw new ApiError(400, 'bad_request', 'url is required for sse/http transport')
  }
  const now = nowIso()
  const server: McpServer = {
    id: uid('mcp'),
    tenant_id: TENANT_ID,
    owner_user_id: body.shared ? null : state.currentUserId ?? DEMO_USER_ID,
    name,
    transport,
    command: body.command?.trim() ?? '',
    args: body.args ? [...body.args] : [],
    env: body.env ? { ...body.env } : {},
    url: body.url?.trim() ?? '',
    headers: body.headers ? { ...body.headers } : {},
    tool_allowlist: body.tool_allowlist ? [...body.tool_allowlist] : [],
    enabled: body.enabled ?? true,
    protocol_version: null,
    server_info: null,
    tools: [],
    last_probe_at: null,
    last_error: '',
    created_at: now,
    updated_at: now,
  }
  state.mcpServers.push(server)
  return clone(server)
}

export async function updateMcpServer(
  id: string,
  body: Partial<McpServerInput>,
): Promise<McpServer> {
  await latency()
  const server = findMcpServer(id)
  if (body.name !== undefined) {
    const name = body.name.trim()
    if (!name) throw new ApiError(400, 'bad_request', 'name is required')
    if (state.mcpServers.some((s) => s.id !== id && s.name === name)) {
      throw new ApiError(409, 'conflict', '同名 MCP server 已存在')
    }
    server.name = name
  }
  if (body.transport !== undefined) server.transport = normalizeTransport(body.transport)
  if (body.command !== undefined) server.command = body.command.trim()
  if (body.args !== undefined) server.args = [...body.args]
  if (body.env !== undefined) server.env = { ...body.env }
  if (body.url !== undefined) server.url = body.url.trim()
  if (body.headers !== undefined) server.headers = { ...body.headers }
  if (body.tool_allowlist !== undefined) server.tool_allowlist = [...body.tool_allowlist]
  if (body.enabled !== undefined) server.enabled = body.enabled
  if (body.shared !== undefined) {
    server.owner_user_id = body.shared ? null : state.currentUserId ?? DEMO_USER_ID
  }
  server.updated_at = nowIso()
  return clone(server)
}

export async function deleteMcpServer(id: string): Promise<void> {
  await latency()
  const index = state.mcpServers.findIndex((s) => s.id === id)
  if (index < 0) throw new ApiError(404, 'not_found', 'MCP server 不存在')
  state.mcpServers.splice(index, 1)
}

/** 探测：mock 里总是成功，回 3 个假工具并刷新 last_probe_at / tools。 */
export async function probeMcpServer(id: string): Promise<McpProbeResult> {
  await latency()
  const server = findMcpServer(id)
  const tools = clone(MOCK_PROBE_TOOLS)
  server.tools = tools
  server.protocol_version = '2025-06-18'
  server.server_info = { name: server.name, version: '1.0.0' }
  server.last_probe_at = nowIso()
  server.last_error = ''
  server.updated_at = server.last_probe_at
  return {
    protocol_version: server.protocol_version ?? undefined,
    server_info: clone(server.server_info),
    tools: clone(tools),
  }
}

/** 跨 server 汇总（只统计 enabled，且受工具白名单过滤）。 */
export async function listMcpTools(): Promise<McpTool[]> {
  await latency()
  const tools: McpTool[] = []
  for (const server of state.mcpServers) {
    if (!server.enabled) continue
    for (const tool of server.tools) {
      if (server.tool_allowlist.length > 0 && !server.tool_allowlist.includes(tool.name)) continue
      tools.push({
        ...clone(tool),
        server: server.name,
        qualified_name: `mcp__${server.name}__${tool.name}`,
      })
    }
  }
  return tools
}

// ── 插件 ────────────────────────────────────────────────────────────────

function readManifest(value: unknown, what: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new ApiError(400, 'bad_request', `${what} manifest must be an object`)
  }
  return clone(value) as Record<string, unknown>
}

function manifestString(manifest: Record<string, unknown>, key: string): string {
  const value = manifest[key]
  return typeof value === 'string' ? value.trim() : ''
}

export async function listPlugins(): Promise<Plugin[]> {
  await latency()
  return clone([...state.plugins].sort((a, b) => a.name.localeCompare(b.name)))
}

export async function createPlugin(body: CreatePluginRequest): Promise<Plugin> {
  await latency()
  const manifest = readManifest(body.manifest, 'plugin')
  const name = manifestString(manifest, 'name')
  if (!name) throw new ApiError(400, 'bad_request', 'plugin manifest must declare a name')
  if (state.plugins.some((p) => p.name === name)) {
    throw new ApiError(409, 'conflict', '同名插件已存在')
  }
  const now = nowIso()
  const plugin: Plugin = {
    id: uid('plg'),
    tenant_id: TENANT_ID,
    owner_user_id: body.shared ? null : state.currentUserId ?? DEMO_USER_ID,
    name,
    kind: manifestString(manifest, 'kind') || 'mcp',
    version: manifestString(manifest, 'version') || '0.0.0',
    source: 'local',
    builtin: false,
    mcp_server_id: body.mcp_server_id ?? null,
    manifest,
    enabled: body.enabled ?? true,
    created_at: now,
    updated_at: now,
  }
  state.plugins.push(plugin)
  return clone(plugin)
}

export async function updatePlugin(id: string, body: UpdatePluginRequest): Promise<Plugin> {
  await latency()
  const plugin = state.plugins.find((p) => p.id === id)
  if (!plugin) throw new ApiError(404, 'not_found', '插件不存在')
  if (body.enabled !== undefined) plugin.enabled = body.enabled
  // 工具白名单与后端一致：写回 `manifest.tools`。
  if (body.tools !== undefined) {
    plugin.manifest = { ...plugin.manifest, tools: [...body.tools] }
  }
  plugin.updated_at = nowIso()
  return clone(plugin)
}

export async function deletePlugin(id: string): Promise<void> {
  await latency()
  const index = state.plugins.findIndex((p) => p.id === id)
  if (index < 0) throw new ApiError(404, 'not_found', '插件不存在')
  state.plugins.splice(index, 1)
}

// ── 技能 ────────────────────────────────────────────────────────────────

export async function listSkills(): Promise<Skill[]> {
  await latency()
  return clone([...state.skills].sort((a, b) => a.name.localeCompare(b.name)))
}

export async function createSkill(body: CreateSkillRequest): Promise<Skill> {
  await latency()
  const manifest = readManifest(body.manifest, 'skill')
  const name = manifestString(manifest, 'name')
  if (!name) throw new ApiError(400, 'bad_request', 'skill manifest must declare a name')
  if (state.skills.some((s) => s.name === name)) {
    throw new ApiError(409, 'conflict', '同名技能已存在')
  }
  const now = nowIso()
  const skill: Skill = {
    id: uid('skl'),
    tenant_id: TENANT_ID,
    owner_user_id: body.shared ? null : state.currentUserId ?? DEMO_USER_ID,
    name,
    version: manifestString(manifest, 'version') || '0.0.0',
    description: manifestString(manifest, 'description'),
    manifest,
    prompt: manifestString(manifest, 'prompt'),
    resources: manifest.resources ?? [],
    scripts: manifest.scripts ?? [],
    source: 'local',
    enabled: body.enabled ?? true,
    created_at: now,
    updated_at: now,
  }
  state.skills.push(skill)
  return clone(skill)
}

export async function updateSkill(id: string, body: UpdateSkillRequest): Promise<Skill> {
  await latency()
  const skill = state.skills.find((s) => s.id === id)
  if (!skill) throw new ApiError(404, 'not_found', '技能不存在')
  if (body.enabled !== undefined) skill.enabled = body.enabled
  if (body.manifest !== undefined) {
    const manifest = readManifest(body.manifest, 'skill')
    // 后端只重新装配 prompt/resources/scripts，name/version/description 不随 PATCH 变更。
    skill.manifest = manifest
    skill.prompt = manifestString(manifest, 'prompt')
    skill.resources = manifest.resources ?? []
    skill.scripts = manifest.scripts ?? []
  }
  skill.updated_at = nowIso()
  return clone(skill)
}

export async function deleteSkill(id: string): Promise<void> {
  await latency()
  const index = state.skills.findIndex((s) => s.id === id)
  if (index < 0) throw new ApiError(404, 'not_found', '技能不存在')
  state.skills.splice(index, 1)
}

// ── 市场 ────────────────────────────────────────────────────────────────

export async function listMarketItems(
  params: { kind?: 'plugin' | 'skill' } = {},
): Promise<MarketItem[]> {
  await latency()
  const kind = params.kind ?? null
  const rows = state.marketItems
    .filter((item) => !kind || item.kind === kind)
    .sort((a, b) => b.published_at.localeCompare(a.published_at))
  return clone(rows)
}

function findMarketItem(itemId: string): MarketItem {
  const item = state.marketItems.find((m) => m.item_id === itemId || m.id === itemId)
  if (!item) throw new ApiError(404, 'not_found', '市场条目不存在')
  return item
}

/** 安装落成 `plugins` / `skills` 行，`source` 固定为 `market:<item_id>`。 */
export async function installMarketItem(
  itemId: string,
  scope?: 'user' | 'tenant',
): Promise<{ installed: boolean; item_id: string }> {
  await latency()
  const item = findMarketItem(itemId)
  const owner = scope === 'tenant' ? null : state.currentUserId ?? DEMO_USER_ID
  const source = `market:${item.item_id}`
  const installed = item.kind === 'plugin'
    ? state.plugins.some((p) => p.source === source && p.owner_user_id === owner)
    : state.skills.some((s) => s.source === source && s.owner_user_id === owner)
  if (installed) throw new ApiError(409, 'conflict', '该条目已安装')

  const now = nowIso()
  const slug = item.name.toLowerCase().replace(/\s+/g, '-')

  if (item.kind === 'plugin') {
    const manifest: Record<string, unknown> = {
      name: item.name,
      version: item.version,
      kind: 'mcp',
      description: item.description,
      mcp: { transport: 'stdio', command: 'npx', args: ['-y', `@market/${slug}`] },
      tools: [],
      permissions: [],
    }
    state.plugins.push({
      id: uid('plg'),
      tenant_id: TENANT_ID,
      owner_user_id: owner,
      name: item.name,
      kind: 'mcp',
      version: item.version,
      source,
      builtin: false,
      mcp_server_id: null,
      manifest,
      enabled: true,
      created_at: now,
      updated_at: now,
    })
  } else {
    const prompt = `按「${item.name}」的约定执行：${item.description}`
    state.skills.push({
      id: uid('skl'),
      tenant_id: TENANT_ID,
      owner_user_id: owner,
      name: item.name,
      version: item.version,
      description: item.description,
      manifest: {
        name: item.name,
        version: item.version,
        description: item.description,
        prompt,
        resources: [],
        scripts: [],
      },
      prompt,
      resources: [],
      scripts: [],
      source,
      enabled: true,
      created_at: now,
      updated_at: now,
    })
  }

  return { installed: true, item_id: item.item_id }
}

export async function uninstallMarketItem(
  itemId: string,
  scope?: 'user' | 'tenant',
): Promise<UninstallMarketItemResult> {
  await latency()
  const item = findMarketItem(itemId)
  const owner = scope === 'tenant' ? null : state.currentUserId ?? DEMO_USER_ID
  const source = `market:${item.item_id}`
  let removed = 0
  if (item.kind === 'plugin') {
    const before = state.plugins.length
    state.plugins = state.plugins.filter((p) => !(p.source === source && p.owner_user_id === owner))
    removed = before - state.plugins.length
  } else {
    const before = state.skills.length
    state.skills = state.skills.filter((s) => !(s.source === source && s.owner_user_id === owner))
    removed = before - state.skills.length
  }
  return { removed }
}

// ── 成员（owner/admin） ─────────────────────────────────────────────────

function tempPassword(): string {
  const alphabet = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789'
  let body = ''
  for (let i = 0; i < 8; i += 1) {
    body += alphabet[Math.floor(Math.random() * alphabet.length)]
  }
  return `Tmp!${body}`
}

function findManagedUser(id: string): User {
  const user = state.users.find((u) => u.id === id && u.status !== 'deleted')
  if (!user) throw new ApiError(404, 'not_found', '成员不存在')
  return user
}

function activeOwnerCount(): number {
  return state.users.filter((u) => u.role === 'owner' && u.status !== 'deleted').length
}

function assertNotLastOwner(user: User): void {
  if (user.role === 'owner' && activeOwnerCount() <= 1) {
    throw new ApiError(403, 'forbidden', '不能停用或删除最后一个 owner')
  }
}

export async function listUsers(params: ListUsersParams = {}): Promise<ListUsersResult> {
  await latency()
  const perPage = Math.min(Math.max(params.per_page ?? 20, 1), 100)
  const page = Math.max(params.page ?? 1, 1)
  // 软删除的账号不出现在列表里（与后端一致）。
  const rows = state.users.filter((u) => u.status !== 'deleted')
  const start = (page - 1) * perPage
  return {
    users: clone(rows.slice(start, start + perPage)),
    total: rows.length,
    page,
    per_page: perPage,
  }
}

export async function createUser(body: CreateUserRequest): Promise<CreateUserResult> {
  await latency()
  const email = body.email?.trim().toLowerCase() ?? ''
  if (!email) throw new ApiError(400, 'bad_request', '请输入邮箱')
  if (!/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) {
    throw new ApiError(400, 'bad_request', '邮箱格式不正确')
  }
  if (state.users.some((u) => u.status !== 'deleted' && u.email === email)) {
    throw new ApiError(409, 'conflict', '邮箱已被占用')
  }
  const username = body.username?.trim().toLowerCase() || null
  if (username && state.users.some((u) => u.status !== 'deleted' && u.username === username)) {
    throw new ApiError(409, 'conflict', '用户名已被占用')
  }
  const role = (body.role ?? 'user').trim().toLowerCase()
  if (role !== 'user' && role !== 'admin' && role !== 'owner') {
    throw new ApiError(400, 'bad_request', 'invalid role')
  }
  const initialPassword = body.initial_password?.trim() ?? ''
  if (initialPassword && initialPassword.length < 8) {
    throw new ApiError(400, 'weak_password', '初始密码至少 8 位')
  }
  const nickname = body.nickname?.trim() ?? ''
  const displayName = body.name?.trim() || nickname || username || email.split('@')[0]
  const user: User = {
    id: uid('usr'),
    email,
    username,
    name: displayName,
    nickname,
    role,
    status: 'active',
    must_change_password: true,
    password_set: true,
    last_login_at: null,
  }
  state.users.push(user)
  state.plans[user.id] = DEMO_PLAN
  return { user: clone(user), initial_password: initialPassword || tempPassword() }
}

export async function resetUserPassword(id: string): Promise<ResetUserPasswordResult> {
  await latency()
  const user = findManagedUser(id)
  user.must_change_password = true
  user.password_set = true
  return { temporary_password: tempPassword() }
}

export async function disableUser(id: string): Promise<void> {
  await latency()
  const user = findManagedUser(id)
  assertNotLastOwner(user)
  user.status = 'disabled'
}

export async function enableUser(id: string): Promise<void> {
  await latency()
  const user = findManagedUser(id)
  user.status = 'active'
}

/** mock 没有真实会话表：只校验成员存在，前端按「已强制下线」处理。 */
export async function forceLogout(id: string): Promise<void> {
  await latency()
  findManagedUser(id)
}

export async function deleteUser(id: string): Promise<void> {
  await latency()
  const user = findManagedUser(id)
  assertNotLastOwner(user)
  // 软删除：状态置 deleted，并像后端一样改写邮箱/用户名释放占用。
  user.status = 'deleted'
  user.email = `deleted+${user.id}@invalid.local`
  user.username = `deleted_${user.id}`
}

// ── 用量 / 审计（mock 聚合） ────────────────────────────────────────────

let auditSeed: AuditEntry[] | null = null

function ensureAudit(): AuditEntry[] {
  if (!auditSeed) {
    const actions = [
      'login_success',
      'password_changed',
      'user_created',
      'user_disabled',
      'mcp.server.probe',
      'market.install',
      'session_created',
    ]
    auditSeed = Array.from({ length: 60 }, (_, i) => ({
      id: 5000 - i,
      tenant_id: 1,
      user_id: i % 4 === 0 ? null : `usr_${(i % 3) + 1}`,
      action: actions[i % actions.length],
      target: i % 2 === 0 ? 'acme' : '',
      detail: `mock audit #${5000 - i}`,
      ip: i % 5 === 0 ? '203.0.113.7' : '127.0.0.1',
      created_at: new Date(Date.now() - i * 37 * 60_000).toISOString(),
    }))
  }
  return auditSeed
}

export async function getUsage(params: UsageParams = {}): Promise<UsageReport> {
  await latency()
  const days = Math.min(90, Math.max(1, params.days ?? 7))
  const scale = Math.max(1, Math.round(days / 7))
  const to = new Date()
  const from = new Date(to.getTime() - days * 86_400_000)
  const models: UsageModelRow[] = [
    {
      model: 'openai:gpt-5.2',
      runs: 42 * scale,
      sessions: 9 * scale,
      input_tokens: 1_250_000 * scale,
      output_tokens: 380_000 * scale,
      cache_read_input_tokens: 120_000 * scale,
      cache_creation_input_tokens: 40_000 * scale,
      cost: 12.5 * scale,
    },
    {
      model: 'anthropic:claude-sonnet-4-6',
      runs: 18 * scale,
      sessions: 6 * scale,
      input_tokens: 730_000 * scale,
      output_tokens: 240_000 * scale,
      cache_read_input_tokens: 60_000 * scale,
      cache_creation_input_tokens: 20_000 * scale,
      cost: 9.8 * scale,
    },
    {
      model: 'deepseek:deepseek-chat',
      runs: 11 * scale,
      sessions: 4 * scale,
      input_tokens: 320_000 * scale,
      output_tokens: 150_000 * scale,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      cost: 0.9 * scale,
    },
  ]
  const users: UsageUserRow[] = [
    {
      user_id: 'usr_1',
      email: 'demo@example.com',
      nickname: '清和',
      runs: 35 * scale,
      sessions: 12 * scale,
      input_tokens: 1_180_000 * scale,
      output_tokens: 420_000 * scale,
      cost: 13.4 * scale,
    },
    {
      user_id: 'usr_2',
      email: 'admin@example.com',
      nickname: '予安',
      runs: 22 * scale,
      sessions: 8 * scale,
      input_tokens: 760_000 * scale,
      output_tokens: 210_000 * scale,
      cost: 7.2 * scale,
    },
    {
      user_id: 'usr_3',
      email: 'member@example.com',
      nickname: '晚晚',
      runs: 14 * scale,
      sessions: 5 * scale,
      input_tokens: 360_000 * scale,
      output_tokens: 140_000 * scale,
      cost: 2.6 * scale,
    },
  ]
  const byTool: UsageToolRow[] = [
    { name: 'exec', calls: 128 * scale, failures: 3 * scale, total_ms: 184_000 * scale, avg_ms: 1437.5, max_ms: 12_000 },
    { name: 'read_file', calls: 96 * scale, failures: 0, total_ms: 21_000 * scale, avg_ms: 218.75, max_ms: 980 },
    { name: 'search_docs', calls: 54 * scale, failures: 2 * scale, total_ms: 32_000 * scale, avg_ms: 592.6, max_ms: 2_400 },
  ]
  const totals = models.reduce(
    (acc, row) => ({
      runs: acc.runs + row.runs,
      sessions: 0,
      input_tokens: acc.input_tokens + row.input_tokens,
      output_tokens: acc.output_tokens + row.output_tokens,
      total_tokens: acc.total_tokens + row.input_tokens + row.output_tokens,
      cache_read_input_tokens:
        acc.cache_read_input_tokens + row.cache_read_input_tokens,
      cache_creation_input_tokens:
        acc.cache_creation_input_tokens + row.cache_creation_input_tokens,
      cost: acc.cost + row.cost,
    }),
    {
      runs: 0,
      sessions: 0,
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      cost: 0,
    },
  )
  totals.sessions = users.reduce((sum, row) => sum + row.sessions, 0)
  const toolTotals = byTool.reduce(
    (acc, row) => ({
      runs: 0,
      sessions: 0,
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      cost: 0,
      calls: acc.calls + row.calls,
      failures: acc.failures + row.failures,
      total_ms: acc.total_ms + row.total_ms,
      avg_ms: 0,
      max_ms: Math.max(acc.max_ms, row.max_ms),
    }),
    {
      runs: 0,
      sessions: 0,
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
      cache_read_input_tokens: 0,
      cache_creation_input_tokens: 0,
      cost: 0,
      calls: 0,
      failures: 0,
      total_ms: 0,
      avg_ms: 0,
      max_ms: 0,
    },
  )
  toolTotals.avg_ms = toolTotals.calls > 0 ? toolTotals.total_ms / toolTotals.calls : 0
  return {
    window: { from: from.toISOString(), to: to.toISOString() },
    totals,
    by_model: models,
    by_user: users,
    tools: {
      totals: {
        calls: toolTotals.calls,
        failures: toolTotals.failures,
        total_ms: toolTotals.total_ms,
        avg_ms: toolTotals.avg_ms,
        max_ms: toolTotals.max_ms,
      },
      by_tool: byTool,
    },
  }
}

export async function listAudit(params: AuditParams = {}): Promise<ListAuditResult> {
  await latency()
  let rows = ensureAudit()
  if (params.action) {
    const action = params.action.toLowerCase()
    rows = rows.filter((row) => row.action.toLowerCase().includes(action))
  }
  if (params.from) {
    const from = Date.parse(params.from)
    if (!Number.isNaN(from)) rows = rows.filter((row) => Date.parse(row.created_at) >= from)
  }
  if (params.to) {
    const to = Date.parse(params.to)
    if (!Number.isNaN(to)) rows = rows.filter((row) => Date.parse(row.created_at) < to)
  }
  const start = Number(params.cursor ?? 0) || 0
  const limit = Math.min(100, Math.max(1, params.limit ?? 20))
  const page = rows.slice(start, start + limit)
  const next = start + limit < rows.length ? String(start + limit) : null
  return { audit: page, next_cursor: next }
}

// ── 媒体 / 文件库（网盘，mock） ─────────────────────────────────────────

const MAX_MEDIA_BYTES = 50 * 1024 * 1024
const PIXEL_PNG =
  'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
const NOTE_TXT = `data:text/plain;base64,${btoa('Chiron mock media\n')}`

let mediaSeed: MediaAsset[] | null = null
const mediaObjectUrls = new Set<string>()

function ensureMedia(): MediaAsset[] {
  if (!mediaSeed) {
    const now = Date.now()
    mediaSeed = [
      {
        id: 'med_seed_pixel',
        filename: 'pixel.png',
        mime: 'image/png',
        kind: 'image',
        bytes: 70,
        sha256: 'seed',
        created_at: new Date(now - 2 * 3600_000).toISOString(),
        url: PIXEL_PNG,
      },
      {
        id: 'med_seed_note',
        filename: 'release-notes.txt',
        mime: 'text/plain',
        kind: 'file',
        bytes: 18,
        sha256: 'seed',
        created_at: new Date(now - 26 * 3600_000).toISOString(),
        url: NOTE_TXT,
      },
    ]
  }
  return mediaSeed
}

export function mediaUrl(id: string): string {
  return ensureMedia().find((item) => item.id === id)?.url ?? ''
}

export async function listMedia(params: ListMediaParams = {}): Promise<ListMediaResult> {
  await latency()
  const rows = [...ensureMedia()].sort((a, b) => b.created_at.localeCompare(a.created_at))
  const start = Number(params.cursor ?? 0) || 0
  const limit = Math.min(100, Math.max(1, params.limit ?? 50))
  const page = rows.slice(start, start + limit)
  return {
    media: page.map((item) => ({ ...item })),
    next_cursor: start + limit < rows.length ? String(start + limit) : null,
  }
}

export async function uploadMedia(file: Blob, filename?: string): Promise<MediaAsset> {
  await latency()
  if (!file || file.size === 0) throw new ApiError(400, 'bad_request', '空文件')
  if (file.size > MAX_MEDIA_BYTES) throw new ApiError(413, 'payload_too_large', '文件超过 50 MiB')
  const mime = (file.type || 'application/octet-stream').split(';')[0].trim().toLowerCase()
  const inline = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(mime)
  const safeName = (filename ?? (file instanceof File ? file.name : 'upload.bin'))
    .replace(/[\\/]/g, '')
    .slice(0, 160) || 'upload.bin'
  const url = URL.createObjectURL(file)
  mediaObjectUrls.add(url)
  const asset: MediaAsset = {
    id: uid('med'),
    filename: safeName,
    mime: inline ? mime : 'application/octet-stream',
    kind: inline ? 'image' : 'file',
    bytes: file.size,
    sha256: uid('sha'),
    created_at: nowIso(),
    url,
  }
  ensureMedia().push(asset)
  return { ...asset }
}

export async function deleteMedia(id: string): Promise<void> {
  await latency()
  const rows = ensureMedia()
  const index = rows.findIndex((item) => item.id === id)
  if (index < 0) throw new ApiError(404, 'not_found', '媒体不存在')
  const [removed] = rows.splice(index, 1)
  if (mediaObjectUrls.has(removed.url)) {
    URL.revokeObjectURL(removed.url)
    mediaObjectUrls.delete(removed.url)
  }
}

/** 退出登录并清空内存数据（调试用；页面刷新也会重置）。 */
export function resetMock(): void {
  listeners.clear()
  seqBySession.clear()
  runControls.clear()
  auditSeed = null
  for (const url of mediaObjectUrls) URL.revokeObjectURL(url)
  mediaObjectUrls.clear()
  mediaSeed = null
  seed()
}
