# LastCommit

LastCommit monitors authenticated activity in your GitHub org. If no trusted
maintainer commits for a configured number of days, it executes your continuity
plan, such as making selected private repositories public.

Your code should not disappear with you.

It is a Rust Cloudflare Worker because you should know your last commit is
memory safe, even in death.

<p align="center">
  <a href="https://lastcommit.xyz"><strong>lastcommit.xyz</strong></a>
</p>

<p align="center">
  <a href="https://github.com/wavey-ai/lastcommit">
    <img src="assets/lastcommit-switch.png" alt="LastCommit switch concept: STILL HERE / LIGHTS OUT. Built with Rust. Armed for absence." width="760">
  </a>
</p>

## Cloudflare fit

Cloudflare Workers Free can run Cron Triggers, and the free account limit is
currently 5 cron triggers. Workers secrets are the right place for the GitHub
credential and the manual-run admin token.

This is not a 100% guaranteed free service. No free platform can promise that
forever. Cloudflare is the best fit here because the cron scheduler, code, and
secrets live outside GitHub. GitHub Actions can run schedules too, but using
GitHub to decide whether GitHub should release your repos is a weaker continuity
boundary.

## Endpoints

- `GET /` - black-background splash page with the LastCommit switch.
- `GET /healthz` - public liveness and redacted config shape.
- `GET /deadz` - public traffic-light status from the last cron or manual run.
  It does not call GitHub.
- `GET /dead` - alias for `GET /deadz`.
- `POST /run` - authenticated manual execution path that refreshes the cached
  status. It still dry-runs unless `LASTCOMMIT_ARMED=true`.

Use:

```bash
curl https://lastcommit.xyz/deadz
```

Public response shape:

```json
{
  "service": "LastCommit",
  "light": "green",
  "status": "alive",
  "message": "Trusted maintainer activity was found.",
  "checkedAt": "2026-06-04T09:17:00Z"
}
```

Traffic lights:

- `green` - trusted maintainer commit activity was found.
- `yellow` - the last check failed, has not run, or setup is blocked.
- `red` - no trusted maintainer commits were found inside the configured window.

## Configuration

Set variables in `wrangler.toml`:

```toml
[vars]
GITHUB_ORG = "wavey-ai"
INACTIVE_DAYS = "180"
TRUSTED_LOGINS = "maintainer-one,maintainer-two"
WATCH_REPOS = "important-private-repo,another-private-repo"
RELEASE_REPOS = "important-private-repo,another-private-repo"
LASTCOMMIT_ARMED = "false"
GITHUB_API_BASE = "https://api.github.com"
```

`WATCH_REPOS` controls which repos count as maintainer life-signs. Keep this
explicit on the Workers Free plan so one check does not burn too many outbound
GitHub requests.

`RELEASE_REPOS` controls what becomes public. Use explicit repo names first.
`RELEASE_REPOS="*"` means all private repos visible to the GitHub token.

`LASTCOMMIT_ARMED=false` is the default and only reports what would happen.
Change it to `true` only after reviewing `/deadz`.

## Secrets

Never put tokens in `wrangler.toml`.

```bash
npx wrangler@4.93.0 secret put GITHUB_TOKEN
npx wrangler@4.93.0 secret put LASTCOMMIT_ADMIN_TOKEN
```

## Status cache

`/deadz` and `/dead` read the last status returned by cron from Workers KV and
reduce it to a public traffic-light response. Create a namespace and copy the
returned ID into `wrangler.toml`:

```bash
npx wrangler@4.93.0 kv namespace create LASTCOMMIT_STATUS
```

Then replace:

```toml
[[kv_namespaces]]
binding = "LASTCOMMIT_STATUS"
id = "replace-with-lastcommit-status-kv-namespace-id"
```

Cron and `POST /run` write the detailed `lastcommit:deadman-status` key.
`GET /deadz` returns `503` until that key exists.

For the MVP, `GITHUB_TOKEN` must be able to read the watched repos and update
the selected repos. A fine-grained token should be limited to the selected org
and repos, with metadata and contents read access plus administration write
access for repos that may be made public.

For a production continuity product, a GitHub App is cleaner than a personal
access token because installation scope and rotation are easier to reason about.

## Local checks

```bash
cargo test
npm install
npm run dev
```

Run ignored live endpoint tests against `wrangler dev` or a deployed Worker:

```bash
LASTCOMMIT_WORKER_BASE=http://localhost:8787 cargo test -- --ignored
```

With `wrangler dev --test-scheduled`, trigger the cron locally:

```bash
curl http://localhost:8787/__scheduled
```

Deploy:

```bash
npx wrangler@4.93.0 deploy
```

## Guardrails

LastCommit treats empty maintainer or release-repo config as blocked, not as
permission to guess. `/deadz` is public, never executes actions, and only
returns a traffic-light summary of the cached cron/manual status. `/run` and
scheduled cron only make repos public when `LASTCOMMIT_ARMED=true`.
