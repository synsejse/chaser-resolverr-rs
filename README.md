# chaser-resolverr-rs

A FlareSolverr-compatible HTTP API (`/v1`, `/health`, `/metrics`, `/`) that
bypasses Cloudflare via a vendored, patched
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
crates/chaser-cf               ← vendored chaser-cf, local patches:
       │     – wait_for_clearance reports its outcome (solved / non-CF /
       │       timed-out / terminal-block) so callers never serve an
       │       unsolved challenge page as success
       │     – get_source returns the real HTTP status
       │     – post_source: browser-native fetch() POST through the page
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
| `RUST_LOG` | `info` | `tracing-subscriber` env filter (e.g. `chaser_resolverr_rs=debug,chaser_cf=info`) |
| `CHASER_*` | varies | Forwarded to the vendored chaser-cf (`CHASER_HEADLESS`, `CHASER_VIRTUAL_DISPLAY`, `CHASER_TIMEOUT`, `CHASER_CONTEXT_LIMIT`, `CHASER_PROFILE`, `CHASER_EXTRA_ARGS`, `CHROME_BIN`) |

## API

FlareSolverr-compatible. Commands handled by `/v1`:

- `request.get` — solve + fetch a URL
- `request.post` — solve + POST an `application/x-www-form-urlencoded` body
  (`postData`), issued through the browser's own `fetch()` so it passes CF's
  TLS/HTTP fingerprinting
- `sessions.create` / `sessions.list` / `sessions.destroy`

`request.get`/`request.post` responses include `solution.url`,
`solution.status` (real HTTP status when Chrome's Performance API surfaces
it, otherwise 200), `solution.response` (HTML/body), `solution.cookies`,
`solution.userAgent`. When the Cloudflare challenge can't be solved (timeout
or a terminal block page), the response is `status: "error"` rather than a
fake `"ok"` wrapping the interstitial HTML.

`maxTimeout` (milliseconds, default 60000) bounds each `request.get`/
`request.post` and also caps how long the per-session lock is held.

Example:

```sh
curl -X POST http://localhost:8191/v1 \
  -H 'Content-Type: application/json' \
  -d '{"cmd":"request.get","url":"https://example.com/"}'
```

`/metrics` exposes Prometheus counters (`chaser_fetches_total`,
`chaser_fetches_failed`); `/health` reports the same counts plus uptime and
last success/failure timestamps.

## Local development

Pure stable Rust (no nightly). `cargo check`, `cargo clippy --all-targets -- -D warnings`,
and `cargo fmt --check` are what CI enforces.

The workspace has two crates:
- `chaser-resolverr-rs` (root) — the FlareSolverr-compat server.
- `crates/chaser-cf` — vendored chaser-cf with our patches. Sync from
  upstream by re-copying the source and re-applying the diff in
  `crates/chaser-cf/src/core/solver.rs` (`wait_for_clearance` outcome
  reporting + interactive/block split, `SourceResponse` status capture,
  `post_source`).

## Caveats

- **No failure screenshots.** Could be added — chromiumoxide's `screenshot` API is available now that we drive the browser ourselves.
