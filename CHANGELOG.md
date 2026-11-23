# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) from `1.0.0` onward; while
pre-1.0, breaking changes may land in minor releases.

## [Unreleased]

### Added

- **Multi-tenant core (S0–S9).** Control database, platform API and console; tenant
  provisioning; Host/JWT double check; per-tenant pools and migrations; quotas and usage
  rollup; BYOK; plan-based sandbox tiers; egress proxy; export, physical delete and
  `pg_dump` backups.
- **Tenant model services (M1).** Per-tenant key pools, model catalog and provider routes
  with runtime failover; remote discovery and health probes; per-user hidden models.
- **OpenCode Go compatibility.** Chat requests now send the tenant session UUID as
  `x-opencode-session` (required by Go routing) and a stable `chiron-agent/0.1` User-Agent
  for both OpenAI- and Anthropic-compatible Go endpoints; Go presets use the documented
  `opencode.ai/inference/go` endpoints and provider clients no longer follow redirects.
- **Tenant workspace (`web/`).** Chat with streaming events, approvals, tools, reasoning,
  display preferences, message anchors and export/share; archive, memory, integrations,
  media, models, usage, audit, members and settings; zh/en strings and mock-data mode.
  Per-conversation settings now cover tool authorization, reply style, reply language,
  thinking language and reasoning effort (stored in session metadata).
- **Operations and HA.** Workspace file API/panel, storage usage, local/WebDAV/S3 media,
  `/metrics`, failure/capacity Webhooks, scheduled backups, TLS ask/probe, platform-admin
  TOTP, Redis Sentinel failover, and multi-region main/standby (region binding, `PATCH
  region`, dump/restore failover scripts).
- **Quality tooling.** i18n AST guard, CI web job, k6 scenarios, mock/local-capacity
  benchmark, and a 21-case read-only cross-tenant audit matrix.
- **Release gates.** Playwright covers console login/TOTP, tenant creation, TLS probe,
  invoice/reconciliation and Alipay/settings/media/provider flows (`console/e2e`, 7 tests);
  an opt-in live test drives real login, tenant creation, invoice and settings against a
  running server; CI runs a real `pg_dump`/`pg_restore` backup drill.
- **Billing controls.** Plans gain `overage_policy` (`block` by default, `allow` to continue
  past monthly token/cost caps while usage stays billable); console displays it. Monthly
  invoice JSON adds plan + usage lines with an external payment status; a reconciliation
  endpoint compares control rollups with tenant `usage_events`/`runs` and reports drift;
  `POST /invoice/status` stores external payment callbacks; the console tenant page exposes
  invoice and reconciliation queries; a HMAC-signed `/billing/webhook` lets a payment
  provider post status updates without an admin session; Alipay RSA2 page-pay URL generation
  and signed async notifications are supported; Alipay keys can be stored encrypted via
  the platform console instead of env files; alert Webhook/thresholds and all four rate
  limits are also console-configurable and hot-reload without restart; media WebDAV/S3
  credentials and platform provider fallback keys are stored encrypted in the console.
- **Region automation (partial).** `healthcheck.sh` reports primary/standby readiness and a
  keep/failover decision; `auto-failover.sh` requires consecutive failures and explicit
  `AUTO_FAILOVER=1`/`CONFIRM` before applying; `failover.sh` can call `DNS_SWITCH_HOOK` to
  drive external DNS/reverse-proxy automation after target verification. Platform
  `GET /regions` reports tenant distribution and unbound-region warnings;
  `/regions/backfill` (dry-run by default) assigns unbound tenants; `CHIRON_REGION_STRICT=1`
  fails closed for unbound tenants and rejects empty/default on create/patch;
  `CHIRON_READ_ONLY=1` keeps tenant APIs read-only; the region runbook now includes rollback.
- **Credential sentry (partial).** Egress allows only HTTPS CONNECT:443 by default; plaintext
  HTTP:80 requires `CHIRON_EGRESS_ALLOW_PLAINTEXT=1`, and credential-bearing plaintext headers
  are still refused; `/metrics` exposes allowed/denied egress counters and the watchdog can
  alert on denied spikes. TLS interception remains out of scope.
- **Console dependency hardening.** Console upgraded to Next 16.4.6; `npm audit` reports 0
  vulnerabilities.

### Changed

- **`web_fetch` usability.** Connection resets/timeouts are retried up to three times
  with fresh DNS validation per attempt; HTML responses are reduced to plain text before
  hitting the model (scripts/styles/comments dropped, common entities decoded, default
  20k-character cap via `CHIRON_WEB_TEXT_MAX_CHARS`) instead of forwarding up to 1 MiB of
  markup that blew the 60k-token context budget. The server keeps standard
  `HTTPS_PROXY`/`HTTP_PROXY` egress support for censored or NATed environments.
- **Chat transcript fidelity.** Message history restores tool-card terminal status from
  `tool_calls` and reuses the same `tool_call_id` as live `tool.call`/`tool.result`, so
  reconnects no longer duplicate cards; stopping a run mid-answer persists the partial
  text; tool-only assistant turns no longer render an "(empty message)" placeholder; the
  stream replay handler treats `message.completed.tool_calls` as a count instead of
  crashing on it.
- `docs/` is consolidated around current behavior: ROADMAP is the single source for status
  and next tasks; API/deploy/PRD describe contracts and operations only.
- Repository ships Apache-2.0 `LICENSE`, `NOTICE`, contributing/security/support policy,
  issue forms, PR template, CODEOWNERS and Dependabot.
- Local validation notes: `cargo test --all` (446 tests), local main/standby drill
  (JWT/Host + 50k-row restore), backup restore drill (6 critical tables PASS),
  21/21 cross-tenant audit, 10 concurrent run / 40 SSE baseline.
- **Production hardening.** MCP stdio is deny-by-default (`CHIRON_MCP_COMMANDS` empty);
  sandbox default network and plan egress default are `none` (migration `0013` rewrites
  `bridge`); placeholder `JWT_SECRET` is rejected; `local-session` is desktop-loopback-only
  and rejects proxy headers; TOTP codes cannot be replayed; migration upgrades take a
  transaction-scoped advisory lock; provisioning rotates existing role passwords;
  subagent model selection honors plan/tenant allowlists; backups write a `.partial` dump
  and rename on success; admin container views filter by tenant label; background
  reconciler/event-log jobs iterate per-tenant pools; `web_fetch` pins each hop to its
  validated DNS address; tenant deletion backs up by default and shares a per-tenant lock
  with migrations/backups; `PATCH /tenants/{id}` with `dsn` also derives `db_name`;
  media storage rejects symlinks and migration verifies checksums before deleting sources;
  SSE disables proxy buffering and caps 16 connections per user; platform logins share
  failure counters through Redis; team/service cookies default to `Secure`.
- **Follow-up isolation and DoS hardening.** Tenant-shared MCP/plugin/skill objects are
  writable only by owner/admin; `project_id` is a safe single path segment; MCP HTTP/SSE
  transports refuse redirects and cross-origin SSE endpoints; `git` subcommands are
  shell-quoted; tenant exports drop password hashes and MCP env/headers; backup files are
  0700/0600 and `pg_dump` errors are redacted; alert Webhook URLs are not logged; the
  Next.js same-origin proxy uses the real Host in production and both frontends send CSP/
  clickjacking headers; console Bearer tokens are not persisted in production; Redis event
  queues are bounded and deleted sessions purge stream/seq keys; media upload checks
  storage quota and heavy routes have a rate class; rate-limit tenant scope uses the
  control UUID; tenant pools rebuild when `dsn` changes; suspend quiesces runs/containers/
  DB connections; run leases renew in PG and stop on loss; egress limits ports/concurrency/
  timeouts; CI is read-only and Docker builds use `.dockerignore`; ops scripts keep DSN
  passwords out of argv.

### Security

- Host/JWT double check on every tenant request; token and host tenant mismatch returns 401.
- MCP stdio remains deny-by-default; remote MCP redirects and cross-origin SSE endpoints
  are rejected; shared integration objects are not member-writable.
- Sandbox and egress default to no network; egress filters private/metadata/CGNAT/
  benchmark/NAT64/6to4/Teredo addresses and limits plain HTTP to 80 / CONNECT to 443.
- Platform API with an empty `PLATFORM_HOST` requires a loopback peer; tenant rate limits
  are keyed by control UUID, not the per-database `tenant_id`.
- Tenant exports omit `users.password_hash` and MCP `env`/`headers`; backups are
  directory 0700 / file 0600.
- Provider keys and tenant DSNs are encrypted at rest and never logged or injected into
  sandboxes; sandbox egress defaults to deny-by-allowlist when the plan uses the proxy.
- Media uploads cannot be served with an attacker-controlled content type; S3/WebDAV
  credential errors are redacted.

### Known gaps

- Billing, dual-active multi-region, a credential sentry (egress is CONNECT passthrough)
  and target-environment capacity/RPO validation are not complete.
- Workspace storage bytes are 0 when `SANDBOX_DATA_ROOT` is not mounted into the server.
- Workspace bind mounts have no per-container disk quota; host-volume exhaustion still
  requires filesystem/project quotas at the infrastructure layer.
- `/metrics` is unauthenticated and must be restricted at the reverse proxy.

## Pre-release history

The repository started as a single-tenant, API-only agent server (v1 baseline): accounts,
sessions/runs/messages, SSE event log, tool approvals, subagents, checkpoints and recovery,
memory, MCP/plugins/skills/market, per-user sandbox containers, rate limiting and audit.
The multi-tenant work above replaced its single-database assumptions; that topology is no
longer supported for production use.
