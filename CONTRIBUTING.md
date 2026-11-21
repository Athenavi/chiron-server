# Contributing to Chiron Server

Thanks for taking the time to contribute. This document covers the practical parts:
how to build and test locally, what we expect from a change, and where things live.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md). For security
issues, do **not** open a public issue — see [SECURITY.md](SECURITY.md).

## Development setup

Prerequisites: Rust stable (see `rust-toolchain.toml`), Docker (PostgreSQL/Redis via
compose), Node.js 20+ if you touch `web/` or `console/`.

```bash
docker compose -f docker-compose.dev.yml up -d
cp .env.example .env
cargo run
```

Tests need a reachable PostgreSQL and Redis; the CI workflow (`.github/workflows/ci.yml`)
shows the expected `DATABASE_URL` / `REDIS_URL` / `JWT_SECRET` values.

```bash
make check       # cargo fmt --all (rewrites files) + clippy -D warnings + test
make test        # cargo test --all
cd web && npm run typecheck
cd console && npm run build && npm run test:e2e   # Playwright
```

On Windows, `scripts\check.cmd` runs the same sequence with `fmt --check` instead.

## What a good change looks like

- **Small and focused.** One behavior change per pull request; refactors that are not
  needed for the change belong in a separate PR.
- **Tested at the right level.** Pure logic gets unit tests next to the module; anything
  touching tokens, tenant resolution, SSRF/egress filtering, or quota math needs a
  negative test (what must be rejected), not only the happy path.
- **Migration-safe.** Tenant schema lives in `migrations/` and is embedded at compile
  time; control schema in `migrations_control/`. Never edit an already-applied migration —
  add a new one. Tenant migrations must be forward-only (SQLx has no down migrations;
  rollback is restore-from-backup).
- **Fail closed.** Unknown plan tier, egress mode, host, or media backend must produce an
  explicit error. Do not silently fall back to a default tenant, runtime, or network.
- **No secrets in logs.** Provider keys, DSNs, owner temporary passwords, and tokens must
  never be logged or echoed back; error details in tenant-facing responses stay generic.
- **Docs in sync.** If you change an endpoint, error code, environment variable or
  delivered capability, update the matching doc (`docs/API.md`, `docs/deploy.md`,
  `docs/PRD.md`), `docs/ROADMAP.md` for status/next tasks, and `CHANGELOG.md` in the same PR.

## Code style

- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` must be clean.
- Rust: prefer explicit error mapping into `AppError` with a stable `code`; keep public
  error codes aligned with `docs/API.md`.
- Front-end (`web/`, `console/`): TypeScript strict, no `any` unless unavoidable, user
  facing strings go through the i18n dictionaries (`web/lib/i18n/messages/`), not inline.
- Comments explain *why* a constraint exists (security boundary, ordering requirement),
  not what the next line does.

## Commits and pull requests

- Commit messages follow Conventional Commits, e.g. `feat(egress): ...`,
  `fix(tenant): ...`, `docs(api): ...`. Keep the subject under ~72 characters.
- Fill in the PR template: what changed, why, how it was verified, and which
  isolation/quota/security boundaries are affected.
- CI must pass. Reviewers look for tenant-isolation regressions first.
- Never commit `.env`, `.env.team`, credentials, dumps, or generated `target/` output.

## Reporting bugs and requesting features

Use the issue forms in `.github/ISSUE_TEMPLATE/`. A useful bug report includes the commit
(or version), deployment shape (single DB vs multi-tenant), the failing request with
secrets redacted, and the relevant server log lines.

## License

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE), and that you have the right to submit them.
