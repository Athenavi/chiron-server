# Security Policy

## Supported versions

| Version | Supported |
| :--- | :--- |
| `main` branch | Yes (security fixes land here first) |
| `0.1.x` | Yes, until the next minor release |
| Older snapshots / forks | No |

This project is pre-1.0. There is no LTS branch and no backport guarantee; fixes ship on
`main` and in the next tagged release.

## Reporting a vulnerability

Do **not** open a public issue, discussion, or pull request for a security problem.

- Preferred: GitHub private vulnerability reporting on this repository
  ("Security" tab → "Report a vulnerability").
- Alternative: email **security@chiron.local** (placeholder — replace before public launch).

Please include: affected commit or version, deployment shape (single-DB vs multi-tenant,
which isolation tier), reproduction steps or a proof of concept, and impact assessment.
Redact real credentials, tokens, and customer data.

We aim to acknowledge within 3 business days and to keep you updated while a fix is
prepared. We will credit reporters in the release notes unless you ask otherwise.

## Scope

In scope — anything that breaks a boundary the design claims to enforce:

- Cross-tenant access: Host/JWT mismatch bypass, tenant DB reachable through another
  tenant's credentials, cross-tenant reads in platform API, Redis key collisions.
- Authentication and authorization: token forgery or replay after revocation, privilege
  escalation between `user`/`admin`/`owner`, platform API reachable from a tenant host,
  `must_change_password` bypass.
- SSRF and egress: reaching private/link-local/cloud-metadata addresses through the egress
  proxy, allowlist bypass (DNS rebinding, redirects, IP literals), sandbox escaping its
  network policy.
- Sandbox escape or privilege escalation on the host, container breakout, mounting host
  paths through agent tools.
- Secret exposure: provider keys or DSNs returned by an API, written to logs, or injected
  into sandboxes.
- Quota and isolation-tier bypass: exceeding plan limits without `429`, silently
  downgrading a tenant from its plan's runtime.
- Stored/reflected XSS through the media library or rendered agent output, path traversal
  in media storage or workspace resolution.
- Denial of service that is cheap for the attacker and expensive for the server
  (unbounded payloads, event-log flooding).

Out of scope:

- Misconfiguration on your own deployment (public platform host, weak `JWT_SECRET`,
  `.env` committed by you, Docker socket exposed to untrusted code).
- Vulnerabilities in upstream providers, models, or third-party packages without a
  demonstrated impact on this project (please report those upstream).
- Findings that require an already-compromised host, or that only affect a single local
  development machine with mock data.
- Missing hardening headers or rate-limit thresholds without a concrete exploit.

## Operational expectations

- Rotating `JWT_SECRET` invalidates all sessions; rotating `SECRETS_KEY` makes stored
  tenant DSNs and unclaimed owner passwords unreadable — plan for that.
- The team compose mounts the host Docker socket. Treat the server container as
  host-privileged and never run untrusted code in it directly; use tenant sandboxes.
- Backups (`CHIRON_BACKUP_DIR`) contain full tenant data; store them encrypted and
  access-controlled.
