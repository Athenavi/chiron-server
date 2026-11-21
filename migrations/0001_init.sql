-- Chiron Server 单文件迁移（B0–B11 合并）。
--
-- 由 0001–0008 合并而来；新库一次建全。分段注释保留原始批次，便于对照 PRD。
-- 不要在这里用事务控制语句：sqlx 会把整个文件放在一个事务里执行。


-- ─────────────────────────────────────────────────────────────
-- 0001_init
-- ─────────────────────────────────────────────────────────────
-- 团队账号最小模型：租户 / 用户 / 审计。
-- 与 Go 网关的字段口径保持一致，便于前端或迁移工具复用。

CREATE TABLE IF NOT EXISTS tenants (
    id          smallint PRIMARY KEY,
    name        text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now()
);

INSERT INTO tenants (id, name) VALUES (1, 'default')
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS users (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id             smallint NOT NULL REFERENCES tenants(id),
    email                 text NOT NULL,
    username              text,
    nickname              text,
    name                  text NOT NULL DEFAULT '',
    password_hash         text NOT NULL,
    password_set          boolean NOT NULL DEFAULT true,
    role                  text NOT NULL DEFAULT 'user',
    status                text NOT NULL DEFAULT 'active',
    must_change_password  boolean NOT NULL DEFAULT false,
    password_changed_at   timestamptz,
    last_login_at         timestamptz,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT users_role_check CHECK (role IN ('owner', 'admin', 'user')),
    CONSTRAINT users_status_check CHECK (status IN ('active', 'disabled', 'deleted'))
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_users_tenant_email
    ON users (tenant_id, lower(email));
CREATE UNIQUE INDEX IF NOT EXISTS ux_users_tenant_username
    ON users (tenant_id, lower(username))
    WHERE username IS NOT NULL AND username <> '';
CREATE INDEX IF NOT EXISTS ix_users_tenant_status ON users (tenant_id, status);

CREATE TABLE IF NOT EXISTS audit_logs (
    id          bigserial PRIMARY KEY,
    tenant_id   smallint NOT NULL,
    user_id     uuid,
    action      text NOT NULL,
    target      text NOT NULL DEFAULT '',
    detail      text NOT NULL DEFAULT '',
    ip          text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS ix_audit_logs_tenant_time ON audit_logs (tenant_id, created_at DESC);

-- ─────────────────────────────────────────────────────────────
-- 0002_sessions
-- ─────────────────────────────────────────────────────────────
-- B2：会话 / 消息 / 轮次 / Run / 工具调用 / 审批 + 幂等键。
--
-- 口径与 docs/PRD.md §5 一致；多实例相关字段（lease_owner/lease_until）先落库，
-- 租约逻辑在 B11 实现。事件本身不落库（B2 先进程内回放），B11 换 Redis Stream。

CREATE TABLE IF NOT EXISTS sessions (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          smallint NOT NULL REFERENCES tenants(id),
    user_id            uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    parent_session_id  uuid REFERENCES sessions(id) ON DELETE SET NULL,
    title              text NOT NULL DEFAULT '',
    project_id         text,
    model              text,
    status             text NOT NULL DEFAULT 'active',
    metadata           jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    archived_at        timestamptz,
    deleted_at         timestamptz,
    CONSTRAINT sessions_status_check CHECK (status IN ('active', 'archived', 'deleted'))
);

-- 列表按 updated_at 倒序 + id 破平，游标即 (updated_at, id)。
CREATE INDEX IF NOT EXISTS ix_sessions_owner_updated
    ON sessions (tenant_id, user_id, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_sessions_project
    ON sessions (tenant_id, project_id)
    WHERE project_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS messages (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id  uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id     uuid,
    run_id      uuid,
    role        text NOT NULL,
    kind        text NOT NULL DEFAULT 'chat',
    content     jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT messages_role_check CHECK (role IN ('system', 'user', 'assistant', 'tool')),
    CONSTRAINT messages_kind_check CHECK (kind IN ('chat', 'steer', 'system', 'tool_result'))
);

-- 历史分页按 (created_at, id) 升序；ID 稳定，插入即可见。
CREATE INDEX IF NOT EXISTS ix_messages_session_cursor
    ON messages (session_id, created_at, id);
CREATE INDEX IF NOT EXISTS ix_messages_turn
    ON messages (turn_id)
    WHERE turn_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_messages_run
    ON messages (run_id)
    WHERE run_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS turns (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id       uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_message_id  uuid REFERENCES messages(id) ON DELETE SET NULL,
    status           text NOT NULL DEFAULT 'queued',
    model            text,
    usage            jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at       timestamptz NOT NULL DEFAULT now(),
    started_at       timestamptz,
    finished_at      timestamptz,
    CONSTRAINT turns_status_check CHECK (status IN (
        'queued', 'running', 'waiting_approval', 'waiting_user',
        'completed', 'failed', 'cancelled'
    ))
);
CREATE INDEX IF NOT EXISTS ix_turns_session
    ON turns (session_id, created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS runs (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id   uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id      uuid REFERENCES turns(id) ON DELETE SET NULL,
    status       text NOT NULL DEFAULT 'queued',
    checkpoint   jsonb,
    lease_owner  text,
    lease_until  timestamptz,
    error        jsonb,
    usage        jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    started_at   timestamptz,
    finished_at  timestamptz,
    CONSTRAINT runs_status_check CHECK (status IN (
        'queued', 'running', 'waiting_approval', 'waiting_user',
        'completed', 'failed', 'cancelled'
    ))
);

-- 同一 session 同时最多一个活跃 run：queued/running/waiting_*。
-- 提交接口并发写时由该索引兜底，冲突映射为 `run.busy`（409）。
CREATE UNIQUE INDEX IF NOT EXISTS ux_runs_one_active_per_session
    ON runs (session_id)
    WHERE status IN ('queued', 'running', 'waiting_approval', 'waiting_user');
CREATE INDEX IF NOT EXISTS ix_runs_session_created
    ON runs (session_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_runs_lease
    ON runs (lease_until)
    WHERE status IN ('queued', 'running', 'waiting_approval', 'waiting_user');

CREATE TABLE IF NOT EXISTS tool_calls (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id  uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id      uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    name        text NOT NULL,
    args        jsonb NOT NULL DEFAULT '{}'::jsonb,
    result      jsonb,
    status      text NOT NULL DEFAULT 'pending',
    error       text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT tool_calls_status_check CHECK (status IN (
        'pending', 'awaiting_approval', 'running', 'completed', 'failed', 'cancelled'
    ))
);
CREATE INDEX IF NOT EXISTS ix_tool_calls_run
    ON tool_calls (run_id, created_at, id);

CREATE TABLE IF NOT EXISTS approvals (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id    uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    run_id        uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    tool_call_id  uuid REFERENCES tool_calls(id) ON DELETE CASCADE,
    status        text NOT NULL DEFAULT 'pending',
    decision      text,
    requested_by  uuid,
    decided_by    uuid,
    payload       jsonb NOT NULL DEFAULT '{}'::jsonb,
    expires_at    timestamptz,
    created_at    timestamptz NOT NULL DEFAULT now(),
    decided_at    timestamptz,
    CONSTRAINT approvals_status_check CHECK (status IN (
        'pending', 'resolved', 'expired', 'cancelled'
    )),
    CONSTRAINT approvals_decision_check CHECK (
        decision IS NULL OR decision IN ('allow', 'deny')
    )
);
CREATE INDEX IF NOT EXISTS ix_approvals_run
    ON approvals (run_id, created_at, id);
CREATE INDEX IF NOT EXISTS ix_approvals_pending
    ON approvals (session_id)
    WHERE status = 'pending';

-- 幂等键：`Idempotency-Key` 24h 内返回首次结果（contract §1）。
-- 单节点用 PG 唯一约束即可；B11 可平移到 Redis，接口不变。
CREATE TABLE IF NOT EXISTS idempotency_keys (
    id            bigserial PRIMARY KEY,
    tenant_id     smallint NOT NULL,
    user_id       uuid NOT NULL,
    endpoint      text NOT NULL,
    idem_key      text NOT NULL,
    request_hash  text NOT NULL,
    status_code   smallint NOT NULL DEFAULT 200,
    response      jsonb NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL DEFAULT now() + interval '24 hours'
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_idempotency_scope
    ON idempotency_keys (tenant_id, user_id, endpoint, idem_key);
CREATE INDEX IF NOT EXISTS ix_idempotency_expiry
    ON idempotency_keys (expires_at);

-- messages.turn_id 的 FK 放在 turns 建表之后，避免循环依赖顺序问题。
ALTER TABLE messages
    ADD CONSTRAINT messages_turn_fk
    FOREIGN KEY (turn_id) REFERENCES turns(id) ON DELETE SET NULL;

-- ─────────────────────────────────────────────────────────────
-- 0003_usage_events
-- ─────────────────────────────────────────────────────────────
-- B5：Agent 运行用量台账（PRD §5 `usage_events`）。
-- B10 的统计/限流会在这里聚合；B5 只在 run 终态写一行终值。

CREATE TABLE IF NOT EXISTS usage_events (
    id                          bigserial PRIMARY KEY,
    tenant_id                   smallint NOT NULL REFERENCES tenants(id),
    user_id                     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    session_id                  uuid REFERENCES sessions(id) ON DELETE SET NULL,
    run_id                      uuid REFERENCES runs(id) ON DELETE SET NULL,
    model                       text,
    input_tokens                bigint NOT NULL DEFAULT 0,
    output_tokens               bigint NOT NULL DEFAULT 0,
    cache_read_input_tokens     bigint NOT NULL DEFAULT 0,
    cache_creation_input_tokens bigint NOT NULL DEFAULT 0,
    cost                        numeric(12, 6) NOT NULL DEFAULT 0,
    created_at                  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_usage_events_user_time
    ON usage_events (tenant_id, user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS ix_usage_events_session
    ON usage_events (session_id, created_at DESC)
    WHERE session_id IS NOT NULL;

-- ─────────────────────────────────────────────────────────────
-- 0004_subagents
-- ─────────────────────────────────────────────────────────────
-- B6：子 Agent run 树。
--
-- 口径（PRD §7.5）：
-- - 子 Agent 不是 `runs` 行：它没有独立 turn/会话，只在父 run 下挂一条记录；
-- - `root_run_id` 让"整个子 Agent 树"可以一次查全（深度上限由应用层保证）；
-- - `depth` 从 1 开始，父 run 本身是 0；
-- - 状态机与 runs 同口径，终态（completed/failed/cancelled）粘滞。
--
-- 预算游标（steps/tool_calls/tokens）落 `budget`/`usage`，用于父 run 的累计熔断。

CREATE TABLE IF NOT EXISTS subagent_runs (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_run_id   uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    root_run_id     uuid NOT NULL,
    parent_subagent_id uuid REFERENCES subagent_runs(id) ON DELETE CASCADE,
    session_id      uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    depth           smallint NOT NULL DEFAULT 1,
    agent           text NOT NULL DEFAULT 'general',
    task            text NOT NULL DEFAULT '',
    status          text NOT NULL DEFAULT 'queued',
    model           text,
    budget          jsonb NOT NULL DEFAULT '{}'::jsonb,
    usage           jsonb NOT NULL DEFAULT '{}'::jsonb,
    summary         text,
    error           jsonb,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    started_at      timestamptz,
    finished_at     timestamptz,
    CONSTRAINT subagent_runs_status_check CHECK (status IN (
        'queued', 'running', 'completed', 'failed', 'cancelled'
    )),
    CONSTRAINT subagent_runs_depth_check CHECK (depth >= 1)
);

-- 父 run 的子树（前端展开用）+ 会话级事件转发时的归属查询。
CREATE INDEX IF NOT EXISTS ix_subagent_runs_parent
    ON subagent_runs (parent_run_id, created_at, id);
CREATE INDEX IF NOT EXISTS ix_subagent_runs_root
    ON subagent_runs (root_run_id, created_at, id);
CREATE INDEX IF NOT EXISTS ix_subagent_runs_session
    ON subagent_runs (session_id, created_at DESC);
-- 活跃子 Agent 计数（并发上限判定）；终态不进索引。
CREATE INDEX IF NOT EXISTS ix_subagent_runs_active
    ON subagent_runs (parent_run_id)
    WHERE status IN ('queued', 'running');

-- ─────────────────────────────────────────────────────────────
-- 0005_checkpoints
-- ─────────────────────────────────────────────────────────────
-- B7：检查点（run 快照 + 工作区 Git 快照）。
--
-- 口径（PRD §7.6）：
-- - `seq` 是 run 内单调递增的序号，`(run_id, seq)` 唯一，重复写入用 ON CONFLICT 收敛；
-- - `snapshot` 只存"恢复所需的最小状态"：消息、工具状态、预算游标、provider 游标；
-- - 工作区快照是 git commit sha + ref 名，对象本身在仓库里（CAS 目录由 git 负责）；
-- - `status` 便于 reconciler 区分"已写一半"与"可用"；
-- - `expires_at` 承担保留策略，GC 只删过期行（不碰仍在跑的 run）。

CREATE TABLE IF NOT EXISTS checkpoints (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id           uuid NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    session_id       uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq              integer NOT NULL,
    kind             text NOT NULL DEFAULT 'run',
    label            text NOT NULL DEFAULT '',
    status           text NOT NULL DEFAULT 'ready',
    snapshot         jsonb NOT NULL DEFAULT '{}'::jsonb,
    workspace_commit text,
    workspace_ref    text,
    size_bytes       bigint NOT NULL DEFAULT 0,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL DEFAULT now() + interval '7 days',
    CONSTRAINT checkpoints_kind_check CHECK (kind IN ('run', 'workspace', 'both')),
    CONSTRAINT checkpoints_status_check CHECK (status IN ('created', 'ready', 'restored', 'discarded')),
    CONSTRAINT checkpoints_seq_check CHECK (seq >= 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_checkpoints_run_seq
    ON checkpoints (run_id, seq);
CREATE INDEX IF NOT EXISTS ix_checkpoints_run_created
    ON checkpoints (run_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS ix_checkpoints_expiry
    ON checkpoints (expires_at);

-- 恢复接管是"租约 CAS"：只有 lease 过期（或未占用）的活跃 run 能被接管。
-- runs 上已有 lease_until 索引；这里补一个"最近检查点"查询用的部分索引。
CREATE INDEX IF NOT EXISTS ix_checkpoints_latest_ready
    ON checkpoints (run_id, seq DESC)
    WHERE status IN ('ready', 'restored');

-- 接管次数与最近检查点序号落在 runs 上：reconciler 需要"尝试了几次"来决定
-- 是否放弃自动接管（避免故障 run 无限重启）。两列都可空/有默认，旧代码不受影响。
ALTER TABLE runs ADD COLUMN IF NOT EXISTS recover_attempts integer NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN IF NOT EXISTS last_checkpoint_seq integer;

-- ─────────────────────────────────────────────────────────────
-- 0006_memory
-- ─────────────────────────────────────────────────────────────
-- B8：内存（结构化槽位 + 会话摘要）。
--
-- 口径见 docs/PRD.md §5/§7.7：v1 不引入 embeddings，只做结构化槽位与
-- PG 侧关键字/全文检索的落点。scope 为 `global`（跨会话）或 `session:<uuid>`
-- （会话级摘要/事实）。source/confidence 让注入内容可追溯、可降权。

CREATE TABLE IF NOT EXISTS memories (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   smallint NOT NULL REFERENCES tenants(id),
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    scope       text NOT NULL DEFAULT 'global',
    slot        text NOT NULL,
    content     text NOT NULL,
    source      text NOT NULL DEFAULT 'user',
    confidence  real NOT NULL DEFAULT 1.0,
    session_id  uuid REFERENCES sessions(id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT memories_slot_check
        CHECK (slot IN ('persona', 'preferences', 'facts', 'summary')),
    CONSTRAINT memories_source_check
        CHECK (source IN ('user', 'auto', 'import')),
    CONSTRAINT memories_scope_check
        CHECK (scope = 'global' OR scope LIKE 'session:%'),
    CONSTRAINT memories_confidence_check
        CHECK (confidence >= 0 AND confidence <= 1)
);

CREATE INDEX IF NOT EXISTS ix_memories_owner_scope
    ON memories (tenant_id, user_id, scope, slot);
CREATE INDEX IF NOT EXISTS ix_memories_owner_updated
    ON memories (tenant_id, user_id, updated_at DESC);

-- persona / summary 是单值槽位：同一作用域只保留一条，写入走"覆盖"。
CREATE UNIQUE INDEX IF NOT EXISTS ux_memories_single_slot
    ON memories (tenant_id, user_id, scope, slot)
    WHERE slot IN ('persona', 'summary');

-- 会话删除后，其会话级记忆随之删除；global 槽位不受影响。
CREATE INDEX IF NOT EXISTS ix_memories_session
    ON memories (session_id)
    WHERE session_id IS NOT NULL;

-- ─────────────────────────────────────────────────────────────
-- 0007_integration
-- ─────────────────────────────────────────────────────────────
-- B9：MCP / 插件 / 技能 / 市场（PRD §7.8）。
--
-- 口径：
-- * MCP 是唯一外部边界，插件只是"MCP server + 元数据"的包装；
-- * 技能是声明式 manifest（提示词/资源），脚本不落库，只存相对路径；
-- * 市场行是目录（可用来源白名单过滤），安装动作落到 plugins/skills 行，
--   因此安装/卸载不需要额外的 installs 表。

CREATE TABLE IF NOT EXISTS mcp_servers (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id        smallint NOT NULL REFERENCES tenants(id),
    -- NULL = 租户共享（owner/admin 创建）；否则是个人 server。
    owner_user_id    uuid REFERENCES users(id) ON DELETE CASCADE,
    name             text NOT NULL,
    -- stdio | sse | http
    transport        text NOT NULL DEFAULT 'stdio',
    command          text NOT NULL DEFAULT '',
    args             jsonb NOT NULL DEFAULT '[]'::jsonb,
    env              jsonb NOT NULL DEFAULT '{}'::jsonb,
    url              text NOT NULL DEFAULT '',
    headers          jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- 工具名白名单：空数组表示"全部允许"（仍受 enabled 与租户边界约束）。
    tool_allowlist   jsonb NOT NULL DEFAULT '[]'::jsonb,
    enabled          boolean NOT NULL DEFAULT true,
    protocol_version text,
    server_info      jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- tools/list 的最近一次结果，供 /mcp/tools 与 Agent 装配直接读。
    tools            jsonb NOT NULL DEFAULT '[]'::jsonb,
    last_probe_at    timestamptz,
    last_error       text NOT NULL DEFAULT '',
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT mcp_servers_transport_check CHECK (transport IN ('stdio', 'sse', 'http')),
    CONSTRAINT mcp_servers_name_nonempty CHECK (length(btrim(name)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_mcp_servers_scope_name
    ON mcp_servers (tenant_id, coalesce(owner_user_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(name));

CREATE TABLE IF NOT EXISTS plugins (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      smallint NOT NULL REFERENCES tenants(id),
    owner_user_id  uuid REFERENCES users(id) ON DELETE CASCADE,
    name           text NOT NULL,
    -- v1 只有 mcp；内置 Rust 插件也用这个 kind + builtin 标记。
    kind           text NOT NULL DEFAULT 'mcp',
    version        text NOT NULL DEFAULT '0.0.0',
    -- market:<item_id> / local / builtin
    source         text NOT NULL DEFAULT 'local',
    builtin        boolean NOT NULL DEFAULT false,
    mcp_server_id  uuid REFERENCES mcp_servers(id) ON DELETE SET NULL,
    manifest       jsonb NOT NULL DEFAULT '{}'::jsonb,
    enabled        boolean NOT NULL DEFAULT true,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT plugins_kind_check CHECK (kind IN ('mcp')),
    CONSTRAINT plugins_name_nonempty CHECK (length(btrim(name)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_plugins_scope_name
    ON plugins (tenant_id, coalesce(owner_user_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(name));

CREATE TABLE IF NOT EXISTS skills (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      smallint NOT NULL REFERENCES tenants(id),
    owner_user_id  uuid REFERENCES users(id) ON DELETE CASCADE,
    name           text NOT NULL,
    version        text NOT NULL DEFAULT '0.0.0',
    description    text NOT NULL DEFAULT '',
    -- 声明式 manifest：prompt/resources/scripts 全在里面，按 schema 校验后才落库。
    manifest       jsonb NOT NULL DEFAULT '{}'::jsonb,
    -- 装配后的提示词（运行时只读这一列，避免每次重算）。
    prompt         text NOT NULL DEFAULT '',
    resources      jsonb NOT NULL DEFAULT '[]'::jsonb,
    scripts        jsonb NOT NULL DEFAULT '[]'::jsonb,
    source         text NOT NULL DEFAULT 'local',
    enabled        boolean NOT NULL DEFAULT true,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT skills_name_nonempty CHECK (length(btrim(name)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_skills_scope_name
    ON skills (tenant_id, coalesce(owner_user_id, '00000000-0000-0000-0000-000000000000'::uuid), lower(name));

CREATE TABLE IF NOT EXISTS market_items (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    smallint NOT NULL REFERENCES tenants(id),
    item_id      text NOT NULL,
    name         text NOT NULL,
    -- plugin | skill
    kind         text NOT NULL,
    version      text NOT NULL DEFAULT '0.0.0',
    description  text NOT NULL DEFAULT '',
    -- 来源标识（白名单按它前缀匹配：official / partner:<org> / tenant:<id>）。
    source       text NOT NULL DEFAULT '',
    -- 下载/校验地址都必须在来源白名单内。
    source_url   text NOT NULL DEFAULT '',
    checksum     text NOT NULL DEFAULT '',
    signature    text NOT NULL DEFAULT '',
    manifest     jsonb NOT NULL DEFAULT '{}'::jsonb,
    published_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT market_items_kind_check CHECK (kind IN ('plugin', 'skill')),
    CONSTRAINT market_items_item_id_nonempty CHECK (length(btrim(item_id)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_market_items_item
    ON market_items (tenant_id, item_id);
CREATE INDEX IF NOT EXISTS ix_market_items_kind ON market_items (tenant_id, kind, published_at DESC);

-- ─────────────────────────────────────────────────────────────
-- 0008_multi_instance
-- ─────────────────────────────────────────────────────────────
-- B11：多实例接口所需的两张表。
--
-- 1) `user_containers.node`：容器是节点本地资源，会话按节点亲和（PRD §4.2）。
--    v1 单节点，所有容器在同一节点；字段与查询接口先就绪，供 B12 跨节点路由。
--    表可能由早期环境手工建过，因此先 CREATE IF NOT EXISTS 再补列。
-- 2) `background_jobs`：后台任务（reconciler / GC / 市场同步）的幂等键，
--    `(consumer, idem_key)` 唯一；重复投递/重复消费只生效一次。

CREATE TABLE IF NOT EXISTS user_containers (
    user_id         uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    tenant_id       smallint NOT NULL,
    node            text NOT NULL DEFAULT 'local',
    container_name  text,
    status          text NOT NULL DEFAULT 'unknown',
    metadata        jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_seen_at    timestamptz NOT NULL DEFAULT now()
);

-- 早期环境可能已有不带 node 的 user_containers：幂等补列。
ALTER TABLE user_containers
    ADD COLUMN IF NOT EXISTS node text NOT NULL DEFAULT 'local';
ALTER TABLE user_containers
    ADD COLUMN IF NOT EXISTS metadata jsonb NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE user_containers
    ADD COLUMN IF NOT EXISTS last_seen_at timestamptz NOT NULL DEFAULT now();

CREATE INDEX IF NOT EXISTS ix_user_containers_node
    ON user_containers (node, status);
CREATE INDEX IF NOT EXISTS ix_user_containers_tenant
    ON user_containers (tenant_id, last_seen_at DESC);

CREATE TABLE IF NOT EXISTS background_jobs (
    id          bigserial PRIMARY KEY,
    consumer    text NOT NULL,
    idem_key    text NOT NULL,
    status      text NOT NULL DEFAULT 'done',
    detail      jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS ux_background_jobs_idem
    ON background_jobs (consumer, idem_key);
CREATE INDEX IF NOT EXISTS ix_background_jobs_created
    ON background_jobs (consumer, created_at DESC);
