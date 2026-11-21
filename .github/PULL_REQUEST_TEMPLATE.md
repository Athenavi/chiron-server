## What changed

<!-- One or two sentences. Link the issue if there is one. -->

## Why

<!-- The problem this solves. If it changes behavior, say what the old behavior was. -->

## How it was verified

<!-- Commands run, tests added, manual checks. Do not paste secrets. -->

- [ ] `make check` (fmt + clippy `-D warnings` + tests) passes
- [ ] New/updated tests cover the change, including what must be rejected
- [ ] `cd web && npm run typecheck` (if `web/` changed)

## Boundary impact

- [ ] Tenant isolation (Host resolution, JWT binding, DB-per-tenant, Redis prefixes)
- [ ] Authentication / authorization / RBAC
- [ ] Quota or plan gating
- [ ] Sandbox or egress / network policy
- [ ] Secrets handling (provider keys, DSNs, tokens, logs)
- [ ] None of the above

## Migrations and configuration

- [ ] No schema change
- [ ] New forward-only migration added (`migrations/` or `migrations_control/`)
- [ ] New / changed environment variable — documented in `.env*.example` and `docs/deploy.md`
- [ ] API or error code changed — documented in `docs/API.md`
- [ ] Delivered capability or status changed — updated in `docs/ROADMAP.md`

## Checklist

- [ ] Docs and `CHANGELOG.md` updated where behavior, endpoints, or config changed
- [ ] No `.env`, credentials, dumps, or generated artifacts are committed
- [ ] Unknown values fail closed (no silent fallback to a default tenant/runtime/backend)
