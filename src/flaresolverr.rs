//! FlareSolverr-compatible API server (v3.3.21 wire format).
//!
//! `V1Response`, `Solution`, and `FlaresolverrCookie` must stay
//! byte-compatible with downstream FlareSolverr clients (Prowlarr et al.).

use axum::{
    Router,
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Json as ResponseJson},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::chaser::ChaserClient;
use crate::config::BrowserConfig;
use crate::fetcher::upsert_cookie;
use crate::session::{DEFAULT_SESSION_ID, SessionManager, StoredCookie};

const STATUS_OK: &str = "ok";
const STATUS_ERROR: &str = "error";
const VERSION: &str = "3.3.21";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlaresolverrCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// `-1.0` = session cookie, per FlareSolverr's convention.
    pub expires: f64,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
    pub secure: Option<bool>,
    #[serde(rename = "sameSite")]
    pub same_site: Option<String>,
}

impl From<StoredCookie> for FlaresolverrCookie {
    fn from(c: StoredCookie) -> Self {
        Self {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            expires: c.expires.map_or(-1.0, |e| e as f64),
            http_only: c.http_only,
            secure: Some(c.secure),
            same_site: c.same_site,
        }
    }
}

impl From<FlaresolverrCookie> for StoredCookie {
    fn from(c: FlaresolverrCookie) -> Self {
        let expires = if c.expires < 0.0 {
            None
        } else {
            Some(c.expires as i64)
        };
        Self {
            name: c.name,
            value: c.value,
            domain: c.domain,
            path: c.path,
            expires,
            http_only: c.http_only,
            secure: c.secure.unwrap_or(false),
            same_site: c.same_site,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyParam {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Solution {
    pub url: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub response: String,
    pub cookies: Vec<FlaresolverrCookie>,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V1Request {
    pub cmd: String,
    pub url: Option<String>,
    #[serde(rename = "postData")]
    pub post_data: Option<String>,
    #[serde(rename = "maxTimeout")]
    pub max_timeout: Option<u32>,
    pub proxy: Option<ProxyParam>,
    pub session: Option<String>,
    #[serde(rename = "session_ttl_minutes")]
    pub session_ttl_minutes: Option<u32>,
    pub cookies: Option<Vec<FlaresolverrCookie>>,
    #[serde(rename = "returnOnlyCookies")]
    pub return_only_cookies: Option<bool>,
    // Deprecated since FlareSolverr v2 — accepted and warned, not honored.
    pub headers: Option<Vec<HashMap<String, String>>>,
    #[serde(rename = "userAgent")]
    pub user_agent: Option<String>,
    pub download: Option<bool>,
    #[serde(rename = "returnRawHtml")]
    pub return_raw_html: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V1Response {
    pub status: String,
    pub message: String,
    #[serde(rename = "startTimestamp")]
    pub start_timestamp: u64,
    #[serde(rename = "endTimestamp")]
    pub end_timestamp: u64,
    pub version: String,
    pub solution: Option<Solution>,
    pub session: Option<String>,
    pub sessions: Option<Vec<String>>,
}

impl V1Response {
    fn ok(message: &str) -> Self {
        Self {
            status: STATUS_OK.to_string(),
            message: message.to_string(),
            start_timestamp: 0,
            end_timestamp: 0,
            version: VERSION.to_string(),
            solution: None,
            session: None,
            sessions: None,
        }
    }

    fn error(message: &str) -> Self {
        Self {
            status: STATUS_ERROR.to_string(),
            message: format!("Error: {}", message),
            start_timestamp: 0,
            end_timestamp: 0,
            version: VERSION.to_string(),
            solution: None,
            session: None,
            sessions: None,
        }
    }

    fn with_solution(mut self, solution: Solution) -> Self {
        self.solution = Some(solution);
        self
    }

    fn with_session(mut self, session: String) -> Self {
        self.session = Some(session);
        self
    }

    fn with_sessions(mut self, sessions: Vec<String>) -> Self {
        self.sessions = Some(sessions);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexResponse {
    pub msg: String,
    pub version: String,
    #[serde(rename = "userAgent")]
    pub user_agent: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// `"ok"` when the browser is ready, otherwise a short failure reason.
    /// Kept first + always present for FlareSolverr-client compat — they
    /// only key off this field.
    pub status: String,
    pub version: &'static str,
    pub uptime_seconds: u64,
    pub chaser_cf_ready: bool,
    pub sessions: usize,
    pub fetches_total: u64,
    pub fetches_failed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_failure_reason: Option<String>,
}

/// Counters + timestamps surfaced by `/health`. `record_*` is called from
/// the fetch path on every outcome.
#[derive(Clone)]
struct HealthState {
    started_at: Instant,
    fetches_total: Arc<AtomicU64>,
    fetches_failed: Arc<AtomicU64>,
    /// Last success/failure timestamps behind a single lock, so `/health`
    /// reads them in one acquisition instead of juggling two.
    timestamps: Arc<Mutex<HealthTimestamps>>,
}

#[derive(Default)]
struct HealthTimestamps {
    last_success_at: Option<DateTime<Utc>>,
    last_failure: Option<FailureRecord>,
}

struct FailureRecord {
    at: DateTime<Utc>,
    reason: String,
}

impl HealthState {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            fetches_total: Arc::new(AtomicU64::new(0)),
            fetches_failed: Arc::new(AtomicU64::new(0)),
            timestamps: Arc::new(Mutex::new(HealthTimestamps::default())),
        }
    }

    async fn record_success(&self) {
        self.fetches_total.fetch_add(1, Ordering::Relaxed);
        self.timestamps.lock().await.last_success_at = Some(Utc::now());
        crate::metrics::record_fetch(true);
    }

    async fn record_failure(&self, reason: impl Into<String>) {
        self.fetches_total.fetch_add(1, Ordering::Relaxed);
        self.fetches_failed.fetch_add(1, Ordering::Relaxed);
        self.timestamps.lock().await.last_failure = Some(FailureRecord {
            at: Utc::now(),
            reason: reason.into(),
        });
        crate::metrics::record_fetch(false);
    }
}

#[derive(Clone)]
struct AppState {
    sessions: SessionManager,
    chaser: ChaserClient,
    health: HealthState,
    prometheus: PrometheusHandle,
}

pub struct FlareSolverrAPI {
    state: AppState,
}

impl FlareSolverrAPI {
    pub fn new(browser_cfg: BrowserConfig, data_path: &str, prometheus: PrometheusHandle) -> Self {
        let data_dir = std::path::Path::new(data_path).join("sessions");
        let chaser = browser_cfg.chaser.clone();
        let sessions = SessionManager::new(browser_cfg, data_dir);
        Self {
            state: AppState {
                sessions,
                chaser,
                health: HealthState::new(),
                prometheus,
            },
        }
    }

    pub fn create_router(&self) -> Router {
        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route("/metrics", get(metrics_endpoint))
            .route("/v1", post(v1_handler))
            .with_state(self.state.clone())
    }
}

/// Prometheus text exposition for scraping. Counters mirror `/health`'s
/// `fetches_total` / `fetches_failed`.
async fn metrics_endpoint(State(state): State<AppState>) -> impl IntoResponse {
    (
        [("content-type", "text/plain; version=0.0.4")],
        state.prometheus.render(),
    )
}

async fn index() -> ResponseJson<IndexResponse> {
    debug!("Index endpoint accessed");
    ResponseJson(IndexResponse {
        msg: "FlareSolverr is ready!".to_string(),
        version: VERSION.to_string(),
        user_agent: "That's a secret :)".to_string(),
    })
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    debug!("Health check");

    let chaser_cf_ready = state.chaser.is_ready().await;
    let sessions = state.sessions.list_sessions().await.len();

    let timestamps = state.health.timestamps.lock().await;
    let last_success_at = timestamps.last_success_at.as_ref().map(rfc3339);
    let last_failure_at = timestamps.last_failure.as_ref().map(|f| rfc3339(&f.at));
    let last_failure_reason = timestamps.last_failure.as_ref().map(|f| f.reason.clone());
    drop(timestamps);

    let (http_status, status_text) = if chaser_cf_ready {
        (StatusCode::OK, STATUS_OK.to_string())
    } else {
        warn!("Health check failed: chaser-cf browser not ready");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "chaser-cf browser not ready".to_string(),
        )
    };

    (
        http_status,
        ResponseJson(HealthResponse {
            status: status_text,
            version: env!("CARGO_PKG_VERSION"),
            uptime_seconds: state.health.started_at.elapsed().as_secs(),
            chaser_cf_ready,
            sessions,
            fetches_total: state.health.fetches_total.load(Ordering::Relaxed),
            fetches_failed: state.health.fetches_failed.load(Ordering::Relaxed),
            last_success_at,
            last_failure_at,
            last_failure_reason,
        }),
    )
}

fn rfc3339(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn v1_handler(
    State(state): State<AppState>,
    Json(req): Json<V1Request>,
) -> ResponseJson<V1Response> {
    let sm = &state.sessions;
    let start = now_ms();

    // Capture identifying fields before `req` is moved into a handler, so the
    // completion event can carry the same structured context as the start.
    let cmd = req.cmd.clone();
    let session = req.session.clone().unwrap_or_else(|| "default".to_string());
    let url = req.url.clone().unwrap_or_default();

    info!(cmd = %cmd, session = %session, url = %url, "Incoming request");

    let mut response = match req.cmd.as_str() {
        "request.get" => handle_get(req, sm, &state.health).await,
        "request.post" => handle_post(req, sm, &state.health).await,
        "sessions.create" => handle_create(req, sm).await,
        "sessions.list" => handle_list(sm).await,
        "sessions.destroy" => handle_destroy(req, sm).await,
        "" => V1Response::error("Parameter 'cmd' is mandatory"),
        cmd => V1Response::error(&format!("Unknown command: {}", cmd)),
    };

    response.start_timestamp = start;
    response.end_timestamp = now_ms();

    let duration = (response.end_timestamp - start) as f64 / 1000.0;
    if response.status == STATUS_OK {
        info!(cmd = %cmd, session = %session, elapsed_secs = duration, "Completed");
    } else {
        error!(
            cmd = %cmd,
            session = %session,
            elapsed_secs = duration,
            reason = %response.message,
            "Request failed"
        );
    }

    ResponseJson(response)
}

/// What `run_fetch` should ask the session to do.
enum FetchOp {
    Get { return_only_cookies: bool },
    Post { post_data: String },
}

async fn handle_get(req: V1Request, sm: &SessionManager, health: &HealthState) -> V1Response {
    let url = match req.url {
        Some(ref u) => u.clone(),
        None => return V1Response::error("Parameter 'url' is mandatory for request.get"),
    };
    if req.post_data.is_some() {
        return V1Response::error("Cannot use 'postData' with GET request");
    }

    warn_ignored_params(&req);

    let op = FetchOp::Get {
        return_only_cookies: req.return_only_cookies.unwrap_or(false),
    };
    run_fetch(&req, sm, health, &url, op).await
}

async fn handle_post(req: V1Request, sm: &SessionManager, health: &HealthState) -> V1Response {
    let url = match req.url {
        Some(ref u) => u.clone(),
        None => return V1Response::error("Parameter 'url' is mandatory for request.post"),
    };
    let post_data = match req.post_data {
        Some(ref d) => d.clone(),
        None => return V1Response::error("Parameter 'postData' is mandatory for request.post"),
    };

    warn_ignored_params(&req);

    let op = FetchOp::Post { post_data };
    run_fetch(&req, sm, health, &url, op).await
}

/// Shared GET/POST pipeline: resolve + lock the session, merge any
/// client-supplied cookies, run the fetch under the request's `maxTimeout`,
/// persist, and shape the FlareSolverr response.
async fn run_fetch(
    req: &V1Request,
    sm: &SessionManager,
    health: &HealthState,
    url: &str,
    op: FetchOp,
) -> V1Response {
    let session_id = sm.resolve_session_id(req.session.as_deref());
    let is_default = session_id == DEFAULT_SESSION_ID;

    let handle = match sm.get_session(&session_id).await {
        Some(h) => h,
        None => return V1Response::error(&format!("Session not found: {}", session_id)),
    };

    // Holding this for the full request serializes concurrent calls on
    // the same session id so cookie/UA state can't be clobbered. The
    // maxTimeout below also bounds how long the lock is held.
    let mut session = handle.lock().await;
    session.touch();

    if !is_default {
        session.reload().ok();
    }

    if let Some(extra) = req.cookies.clone() {
        let added = extra.len();
        for incoming in extra {
            upsert_cookie(&mut session.data.cookies, StoredCookie::from(incoming));
        }
        debug!(
            "[session={}] Merged {} client-supplied cookie(s)",
            session_id, added
        );
    }

    debug!(
        "[session={}] Using session ({} cookies)",
        session_id,
        session.data.cookies.len()
    );

    let budget = resolve_max_timeout(req.max_timeout);
    let fetch_result = match op {
        FetchOp::Get {
            return_only_cookies,
        } => with_timeout(budget, session.fetch(url, return_only_cookies)).await,
        FetchOp::Post { ref post_data } => {
            with_timeout(budget, session.fetch_post(url, post_data)).await
        }
    };

    if let Err(e) = session.save() {
        warn!("[session={}] Failed to save: {}", session_id, e);
    }

    match fetch_result {
        Ok(response) => {
            health.record_success().await;
            let solution = Solution {
                url: response.url,
                status: response.status,
                headers: HashMap::new(),
                response: response.body,
                cookies: response
                    .cookies
                    .into_iter()
                    .map(FlaresolverrCookie::from)
                    .collect(),
                user_agent: response.user_agent,
            };

            V1Response::ok("Challenge solved!")
                .with_solution(solution)
                .with_session(session_id)
        }
        Err(e) => {
            health.record_failure(format!("{}: {}", url, e)).await;
            V1Response::error(&format!("Challenge failed: {}", e))
        }
    }
}

/// Run `fut` under `budget`, mapping a timeout to a descriptive error so the
/// `/v1` response reports `status: "error"` instead of blocking indefinitely.
async fn with_timeout<F>(budget: Duration, fut: F) -> anyhow::Result<crate::fetcher::FetchResponse>
where
    F: std::future::Future<Output = anyhow::Result<crate::fetcher::FetchResponse>>,
{
    match tokio::time::timeout(budget, fut).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "request exceeded maxTimeout of {}ms",
            budget.as_millis()
        )),
    }
}

/// FlareSolverr's default solve budget when the client omits `maxTimeout`.
const DEFAULT_MAX_TIMEOUT_MS: u64 = 60_000;

fn resolve_max_timeout(max_timeout: Option<u32>) -> Duration {
    let ms = max_timeout
        .map(u64::from)
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

/// Warn about request params we accept for wire-compat but intentionally
/// don't honor per-request.
fn warn_ignored_params(req: &V1Request) {
    warn_deprecated(req);
    // The upstream proxy is fixed at startup; per-request overrides are
    // warn-and-ignored to keep the wire format FlareSolverr-compatible.
    if req.proxy.is_some() {
        warn!("Per-request 'proxy' is ignored; upstream proxy is configured globally via env");
    }
    if req.session_ttl_minutes.is_some() {
        warn!("'session_ttl_minutes' on request.get/post is ignored; set it on sessions.create");
    }
}

async fn handle_create(req: V1Request, sm: &SessionManager) -> V1Response {
    match sm.create_session(req.session_ttl_minutes).await {
        Ok(id) => V1Response::ok("Session created").with_session(id),
        Err(e) => V1Response::error(&format!("Failed to create session: {}", e)),
    }
}

async fn handle_list(sm: &SessionManager) -> V1Response {
    let sessions = sm.list_sessions().await;
    let count = sessions.len();
    V1Response::ok(&format!("Found {} session(s)", count)).with_sessions(sessions)
}

async fn handle_destroy(req: V1Request, sm: &SessionManager) -> V1Response {
    let id = match req.session {
        Some(ref id) => id.clone(),
        None => return V1Response::error("Parameter 'session' is mandatory for sessions.destroy"),
    };

    match sm.destroy_session(&id).await {
        Ok(()) => V1Response::ok("Session destroyed").with_session(id),
        Err(e) => V1Response::error(&format!("{}", e)),
    }
}

fn warn_deprecated(req: &V1Request) {
    if req.headers.is_some() {
        warn!("Deprecated: 'headers' was removed in FlareSolverr v2");
    }
    if req.user_agent.is_some() {
        warn!("Deprecated: 'userAgent' was removed in FlareSolverr v2");
    }
    if req.return_raw_html.is_some() {
        warn!("Deprecated: 'returnRawHtml' was removed in FlareSolverr v2");
    }
    if req.download.is_some() {
        warn!("Deprecated: 'download' was removed in FlareSolverr v2");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_timeout_defaults_when_absent_or_zero() {
        let default = Duration::from_millis(DEFAULT_MAX_TIMEOUT_MS);
        assert_eq!(resolve_max_timeout(None), default);
        assert_eq!(resolve_max_timeout(Some(0)), default);
    }

    #[test]
    fn max_timeout_honors_explicit_value() {
        assert_eq!(resolve_max_timeout(Some(5000)), Duration::from_millis(5000));
    }

    #[test]
    fn cookie_roundtrip_session_cookie() {
        let stored = StoredCookie {
            name: "cf_clearance".into(),
            value: "abc".into(),
            domain: Some("x.com".into()),
            path: Some("/".into()),
            expires: None,
            http_only: true,
            secure: true,
            same_site: Some("Lax".into()),
        };
        let fs = FlaresolverrCookie::from(stored.clone());
        // FlareSolverr encodes a session cookie as expires = -1.0.
        assert_eq!(fs.expires, -1.0);

        let back = StoredCookie::from(fs);
        assert_eq!(back.expires, None);
        assert!(back.http_only);
        assert!(back.secure);
        assert_eq!(back.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn cookie_roundtrip_with_expiry() {
        let stored = StoredCookie {
            name: "a".into(),
            value: "b".into(),
            domain: None,
            path: None,
            expires: Some(1_700_000_000),
            http_only: false,
            secure: false,
            same_site: None,
        };
        let fs = FlaresolverrCookie::from(stored);
        assert_eq!(fs.expires, 1_700_000_000.0);
        assert_eq!(StoredCookie::from(fs).expires, Some(1_700_000_000));
    }
}
