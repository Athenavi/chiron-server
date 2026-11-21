# Support

This is a community-maintained open-source project. There is no commercial SLA.

## Where to ask

| Need | Where |
| :--- | :--- |
| Bug (reproducible, not security) | GitHub issue → Bug report form |
| Feature idea / scope question | GitHub issue → Feature request form |
| Usage or deployment question | GitHub Discussions (or the issue tracker if Discussions is disabled) |
| Security vulnerability | See [SECURITY.md](SECURITY.md) — private report only |
| Conduct concern | conduct@chiron.local (placeholder) |

## Before you open an issue

1. Read [docs/PRD.md](docs/PRD.md), [docs/API.md](docs/API.md) and
   [docs/deploy.md](docs/deploy.md); most "why does Host X return 404/503" and
   "why is my run stuck" questions are answered there.
2. Check `/readyz` — it reports PostgreSQL, Redis and Docker reachability, plus the
   number of cached tenant pools.
3. Search existing issues, including closed ones.
4. Collect: commit/version, deployment shape (single DB or multi-tenant), the failing
   request (secrets redacted), and the relevant structured log lines.

## What we can help with

- Reproducible bugs in `src/`, `web/`, `console/`, migrations, and the deploy files in
  this repository.
- Clarifying documented behavior and configuration.

## What we cannot help with

- Custom integrations or managed hosting for your deployment.
- Debugging third-party model providers, MCP servers, or packages we do not control.
- Recovering data after `SECRETS_KEY` loss, accidental `DROP`, or missing backups.
- Guaranteed response times.

## Response expectations

Maintainers triage issues when time permits. Security reports are prioritized; see
[SECURITY.md](SECURITY.md) for the acknowledgement target.
