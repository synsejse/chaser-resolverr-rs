# chaser-resolverr-rs

A FlareSolverr-compatible HTTP API (`/v1`, `/health`, `/`) that bypasses
Cloudflare via a vendored, patched
[chaser-cf](https://github.com/0xchasercat/chaser-cf) running in-process.
Drop-in replacement for Prowlarr, Sonarr, Radarr et al. — anything that
speaks the FlareSolverr v3.3.21 wire format.

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
       │  Rust function call
       ▼
crates/chaser-cf               ← vendored chaser-cf, two local patches:
       │     – wait_for_clearance bails when no CF challenge is visible
       │     – get_source returns the real HTTP status
       ▼
Chrome (headful + Xvfb)        ← driven via CDP in-process
```

Each `request.get` calls `solve_waf_session` (to populate `cf_clearance`
+ UA) at most once per session-host-TTL window, then `get_source` for
the rendered HTML.

## Quick start

```sh
docker compose up -d
```

Pulls the prebuilt image from ghcr (`ghcr.io/synsejse/chaser-resolverr-rs:main`).
API is then on `http://localhost:8191`.

To build locally: `docker compose up --build`. The image bundles
chromium + xvfb (~1.4GB) since the browser runs in the same container.

## Configuration

All config is env vars; defaults are usable as-is.

| Variable | Default | Notes |
|---|---|---|
| `HOST` | `0.0.0.0` | Listen interface |
| `PORT` | `8191` | Listen port |
| `DATA_PATH` | `/data` | Where session JSON files live |
| `CLEARANCE_TTL_SECONDS` | `1500` | Cached `cf_clearance` freshness (25 min — cf_clearance defaults to ~30 min) |
| `PROXY_HOST` / `PROXY_PORT` | — | Optional upstream proxy passed through to Chrome |
| `PROXY_USERNAME` / `PROXY_PASSWORD` | — | Optional proxy auth |
| `RUST_LOG` | `info,tracing::span=warn` | Standard `env_logger` filter |
| `CHASER_*` | varies | Forwarded to the vendored chaser-cf (`CHASER_HEADLESS`, `CHASER_VIRTUAL_DISPLAY`, `CHASER_TIMEOUT`, `CHASER_CONTEXT_LIMIT`, `CHASER_PROFILE`, `CHASER_EXTRA_ARGS`, `CHROME_BIN`) |

## API

FlareSolverr-compatible. Commands handled by `/v1`:

- `request.get` — solve + fetch a URL
- `request.post` — explicitly **unimplemented** (returns an error; configure indexers for GET)
- `sessions.create` / `sessions.list` / `sessions.destroy`

`request.get` responses include `solution.url`, `solution.status` (real
HTTP status when Chrome's Performance API surfaces it, otherwise 200),
`solution.response` (HTML), `solution.cookies`, `solution.userAgent`.

Example:

```sh
curl -X POST http://localhost:8191/v1 \
  -H 'Content-Type: application/json' \
  -d '{"cmd":"request.get","url":"https://example.com/"}'
```

## Local development

Pure stable Rust (no nightly). `cargo check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo fmt --check` are what CI enforces.

The workspace has two crates:
- `chaser-resolverr-rs` (root) — the FlareSolverr-compat server.
- `crates/chaser-cf` — vendored chaser-cf with our patches. Sync from
  upstream by re-copying the source and re-applying the diff in
  `crates/chaser-cf/src/core/solver.rs` (`is_cloudflare_challenge_page`
  early-exit + `SourceResponse` status capture).

## Caveats

- **`request.post` is not implemented.** chaser-cf renders only GETs through the real browser.
- **No failure screenshots.** Could be added — chromiumoxide's `screenshot` API is available now that we drive the browser ourselves.
