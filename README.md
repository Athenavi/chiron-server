# Chiron Server

Multi-tenant AI agent backend in Rust. One process serves many customer organizations:
each tenant gets its own PostgreSQL database, users, sandbox tier, egress policy and
model services (key pool, catalog, routing), all resolved from the request `Host` header.
API-only by design, with a Next.js tenant workspace (`web/`) and a platform ops console
(`console/`) in the same repo.

> 中文摘要：Chiron Server 是多租户 AI Agent 后端（Rust + axum + PostgreSQL + Redis）。
> 控制库存租户/域名/套餐/审计，业务数据走 **DB-per-tenant** 物理隔离；请求按 Host 解析
> 租户并与 JWT 绑定；沙箱按套餐分 sysbox / gVisor / microVM；egress 走白名单正向代理；
> 模型服务按租户维护 key 池、目录与路由并支持失败转移。仓库同时包含租户前台（`web/`）
> 与平台控制台（`console/`）。许可证 Apache-2.0。

## Architecture

```text
                     control plane (chiron_control)
     console / platform API ──► tenants · tenant_domains · plans · admins
                                 tenant_usage_daily · tenant_migration_runs
                                 tenant_provider_keys (AES-256-GCM) · platform_audit
                                              │  tenant registry + encrypted DSN
        ┌─────────────────────────────────────┼─────────────────────────────────────┐
        ▼                                     ▼                                     ▼
  tenant A host                        tenant B host                        custom domain host
  /api/v1/*  (Host -> tenant)          /api/v1/*                            /api/v1/*
        │                                     │
        └────────────► chiron-server (data plane, Rust/axum) ◄──────────────────────┘
                          ├─ chiron_t_<slug> database per tenant (pool registry, LRU)
                          ├─ Redis: sessions, leases, events (t:<tenant>: prefix)
                          ├─ sandbox pool: sysbox | gVisor | microVM  (plan tier)
                          └─ egress proxy: CONNECT allowlist, private/metadata blocked
```

- The control plane stores routing, plan and usage facts only; tenant business data never
  leaves the tenant database.
- Every tenant has one `<slug>.<base-domain>` host and at most one verified custom domain.
  Unknown hosts return 404 — there is no default-tenant fallback.
- Tenant JWT and resolved host must agree, otherwise 401.

## Features

- **Tenant lifecycle**: platform-admin-only provisioning (create DB/role, run migrations,
  issue a one-time owner password), suspend/resume, JSON export, physical delete, `pg_dump`
  backups, per-tenant migration upgrade with dry-run.
- **Isolation by construction**: DB-per-tenant, per-tenant Redis key prefixes, tenant UUID
  in sandbox names/labels/workspaces, egress allowlist proxy with DNS-rebinding checks.
- **Plans & quotas**: users, concurrent runs, monthly tokens, monthly cost ceiling, sandbox
  count, workspace storage, model allowlist — checked before submit, `429 quota_exceeded`.
- **Tenant model services**: per-tenant key pools (multiple keys per provider, weight,
  enable/disable), model catalog, provider routes with priority/weight, runtime failover
  across routes and keys, remote model discovery and per-provider health probes. Keys are
  encrypted at rest and never echoed; users see only enabled models and can hide their own.
  Platform `*_API_KEY` env vars remain the fallback when a tenant has no key.
- **Agent runtime**: run state machine, streaming events over SSE with replay/`Last-Event-ID`,
  tool approvals, steer/interrupt, subagents, checkpoints + recovery, memory, MCP servers,
  plugins, skills, market items.
- **Media library**: netdisk-style arbitrary file storage per user (50 MiB/file); local,
  WebDAV or S3 (SigV4, MinIO-compatible) backend; only magic-byte-verified
  PNG/JPEG/WebP/GIF render inline, everything else is a forced download.
- **Workspace files**: browse and stream a session workspace from the run panel, with
  tenant/session auth, path-traversal and symlink guards.
- **Operations**: `/healthz`, `/readyz`, `/metrics`, admin audit/usage/runtime/container
  views, optional platform-admin TOTP, scheduled usage rollup and tenant backups
  (leader-leased), optional failure Webhooks, per-subject and per-tenant rate limiting,
  structured JSON logs.
- **Frontends**: tenant workspace (chat, sessions, memory, integrations, netdisk media,
  model catalog + owner/admin key/route management, usage, audit, members, settings) with
  zh/en UI strings; platform console for tenants, plans, usage, migrations (also zh/en).

## Quick start

### 1. Front-end only (mock data, no backend)

```bash
cd web
npm install
npm run dev          # http://localhost:3000
```

`NEXT_PUBLIC_USE_MOCK` defaults to `1`, so the UI runs against an in-memory mock.
Set `NEXT_PUBLIC_USE_MOCK=0` and `CHIRON_API_ORIGIN` (default `http://127.0.0.1:8088`)
to proxy `/api/v1` to a local server (see `web/app/api/v1/[...path]/route.ts`).

### 2. Backend in dev mode

```bash
docker compose -f docker-compose.dev.yml up -d   # PostgreSQL 16 + Redis 7
cp .env.example .env                             # then adjust secrets
# JWT_SECRET 必填：openssl rand -hex 32；团队模式还需 CHIRON_METRICS_TOKEN
cargo run                                        # migrations run on startup
curl -fsS http://127.0.0.1:8088/healthz
```

On Windows with a non-standard MSVC toolchain, `scripts\cargo-msvc.cmd run` wraps the
`vcvars64` environment (edit the paths inside first). `make check` = fmt + clippy + test.

`.env.example` already sets `CONTROL_DATABASE_URL`, but the dev compose creates only the
`chiron` database — create `chiron_control` or leave the variable empty for a single-DB run.

### 3. Service / multi-tenant stack

```bash
docker build -t chiron-agent:py314 deploy/agent          # sandbox image
cp .env.team.example .env.team                           # fill the values below
docker compose --env-file .env.team -f docker-compose.team.yml up -d --build
curl -fsS http://127.0.0.1:8088/healthz
curl -fsS http://127.0.0.1:8088/readyz
```

Minimum to make multi-tenancy work: `JWT_SECRET`, `POSTGRES_PASSWORD`, `SECRETS_KEY`
(`openssl rand -base64 32`), `CONTROL_DATABASE_URL`, `PLATFORM_HOST`, `TENANT_BASE_DOMAIN`
and one provider key. Losing `SECRETS_KEY` makes tenant DSNs and owner passwords unreadable.

### 4. Platform console

```bash
cd console && npm install && npm run dev      # talks to /api/platform/v1 on the platform host
```

## Repository layout

```text
src/                 Rust backend (API-only)
  control/           control plane: tenants, domains, provisioning, BYOK, usage, backup
  tenant/            Host resolution, pool registry, quotas
  agent/             run loop, tools, approvals, persistence
  sessions/          sessions/runs/messages API, SSE event log, leases
  subagent/          subagent trees and budgets
  checkpoint/        snapshots, restore, reconciler
  memory/ media/     memory stores, netdisk file library
  integration/       MCP, plugins, skills, market
  models/            tenant model services: key pool, catalog, routes, discovery/probes
  provider/          OpenAI/Anthropic protocol clients, catalog, failover orchestration
  egress.rs          tenant egress proxy (CONNECT + allowlist)
migrations/          tenant DB schema (embedded at compile time)
migrations_control/  control DB schema
web/                 Next.js tenant workspace (zh/en) · console/ = platform console
deploy/              Dockerfiles, Caddyfile, control DB init · docs/ = PRD, API, deploy, roadmap
```

## Documentation

- [docs/PRD.md](docs/PRD.md) — product scope, tenant model, data isolation, plans, status.
- [docs/API.md](docs/API.md) — tenant API, platform API, error codes, SSE, Host resolution.
- [docs/deploy.md](docs/deploy.md) — compose, schemas, environment variables, troubleshooting.
- [docs/ROADMAP.md](docs/ROADMAP.md) — the single source for status, P0/P1/P2 tasks and known gaps.
- [CHANGELOG.md](CHANGELOG.md) — released and unreleased changes.

## Project status

S0–S9 core (lifecycle, Host routing, migrations, quotas, BYOK, sandbox tiers, egress,
console, export/backup/delete), tenant model services (M1) and the P1/P2 operations batch
(workspace files, storage usage, on-demand TLS, scheduled backups, S3/WebDAV, metrics,
alerts, platform-admin TOTP, Redis Sentinel HA, multi-region main/standby, load/audit
tooling) are implemented. Missing: billing, dual-active multi-region, a credential sentry
and target-environment capacity/RPO validation. See
[docs/ROADMAP.md](docs/ROADMAP.md) for the next tasks.

## Contributing, security, support

- Contributions: [CONTRIBUTING.md](CONTRIBUTING.md); community rules:
  [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
- Security reports: [SECURITY.md](SECURITY.md) — please do not open public issues.
- Questions and usage help: [SUPPORT.md](SUPPORT.md).
- License: [Apache-2.0](LICENSE); attribution in [NOTICE](NOTICE).
