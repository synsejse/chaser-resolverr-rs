# chaser-resolverr-rs

A FlareSolverr-compatible HTTP API (`/v1`, `/health`, `/`) backed by
[chaser-cf](https://github.com/0xchasercat/chaser-cf) for Cloudflare
challenge solving. Drop-in replacement for clients like Prowlarr, Sonarr,
Radarr that already speak the FlareSolverr v3.3.21 wire format.

## How it fits

```
Prowlarr / Sonarr / …          (FlareSolverr clients)
       │
       │  POST /v1
       ▼
chaser-resolverr-rs            ← this repo
       │   • FlareSolverr v3.3.21 wire format
       │   • on-disk session store (UA + cookies + clearance TTL)
       │   • host-change detection / cache invalidation
       │
       │  POST /solve
       ▼
chaser-cf                      ← Cloudflare solver (separate container)
       │
       ▼
Chrome (headful + Xvfb)
```

Each `request.get` maps to up to two `/solve` calls: one `waf-session`
when our cached `cf_clearance` is missing/stale/wrong-host, then one
`source` for the rendered HTML body.

## Quick start

```sh
docker compose up --build
```

That builds both images (this server + chaser-cf from source, with xvfb
bundled and the rust toolchain bumped — chaser-cf's upstream Dockerfile
pins 1.85, which is too old for current deps) and starts them. API is
then on `http://localhost:8191`.

Override the chaser-cf git ref with a build arg if you want to pin:
`docker compose build --build-arg CHASER_CF_REF=<sha|tag|branch>`.

## Configuration

All config is env vars; defaults are usable for a localhost dev stack.

| Variable | Default | Notes |
|---|---|---|
| `CHASER_URL` | `http://127.0.0.1:3000` | Where to reach the chaser-cf sidecar |
| `CHASER_AUTH_TOKEN` | — | Bearer token if chaser-cf was started with `AUTH_TOKEN` |
| `HOST` | `0.0.0.0` | Listen interface |
| `PORT` | `8191` | Listen port |
| `DATA_PATH` | `/data` | Where session JSON files live |
| `CLEARANCE_TTL_SECONDS` | `1500` | How long a cached `cf_clearance` is considered fresh (25 min — cf_clearance defaults to ~30 min) |
| `PROXY_HOST` / `PROXY_PORT` | — | Optional upstream proxy passed through to chaser-cf |
| `PROXY_USERNAME` / `PROXY_PASSWORD` | — | Optional proxy auth |
| `RUST_LOG` | `info,tracing::span=warn` | Standard `env_logger` filter |

## API

FlareSolverr-compatible. The commands handled by `/v1`:

- `request.get` — solve + fetch a URL
- `request.post` — explicitly **unimplemented** (returns an error; configure indexers for GET)
- `sessions.create` / `sessions.list` / `sessions.destroy`

`request.get` responses include `solution.url`, `solution.status` (always 200 — chaser-cf doesn't surface the upstream status), `solution.response` (HTML), `solution.cookies`, `solution.userAgent`.

Example:

```sh
curl -X POST http://localhost:8191/v1 \
  -H 'Content-Type: application/json' \
  -d '{"cmd":"request.get","url":"https://example.com/","maxTimeout":60000}'
```

## Local development

Pure stable Rust (no nightly). `cargo check`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` are what CI enforces.

## Caveats

- **No real upstream HTTP status.** chaser-cf's HTTP API doesn't expose it; we return 200 on every successful body fetch. See [issue/PR upstream](https://github.com/0xchasercat/chaser-cf) if you want this fixed properly.
- **No failure screenshots.** The browser lives in the chaser-cf sidecar, and chaser-cf's API doesn't ship a screenshot mode.
- **`request.post` is not implemented** — chaser-cf only renders GETs through a real browser.
