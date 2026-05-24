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
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::Mutex;

use crate::chaser::ChaserClient;
use crate::config::BrowserConfig;
use crate::fetcher::upsert_cookie;
use crate::logging::LogContext;
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
/// `handle_get` on every fetch outcome.
#[derive(Clone)]
struct HealthState {
    started_at: Instant,
    fetches_total: Arc<AtomicU64>,
    fetches_failed: Arc<AtomicU64>,
    last_success_at: Arc<Mutex<Option<DateTime<Utc>>>>,
    last_failure: Arc<Mutex<Option<FailureRecord>>>,
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
            last_success_at: Arc::new(Mutex::new(None)),
            last_failure: Arc::new(Mutex::new(None)),
        }
    }

    async fn record_success(&self) {
        self.fetches_total.fetch_add(1, Ordering::Relaxed);
        *self.last_success_at.lock().await = Some(Utc::now());
    }

    async fn record_failure(&self, reason: impl Into<String>) {
        self.fetches_total.fetch_add(1, Ordering::Relaxed);
        self.fetches_failed.fetch_add(1, Ordering::Relaxed);
        *self.last_failure.lock().await = Some(FailureRecord {
            at: Utc::now(),
            reason: reason.into(),
        });
    }
}

#[derive(Clone)]
struct AppState {
    sessions: SessionManager,
    chaser: ChaserClient,
    health: HealthState,
}

pub struct FlareSolverrAPI {
    state: AppState,
}

impl FlareSolverrAPI {
    pub fn new(browser_cfg: BrowserConfig, data_path: &str) -> Self {
        let data_dir = std::path::Path::new(data_path).join("sessions");
        let chaser = browser_cfg.chaser.clone();
        let sessions = SessionManager::new(browser_cfg, data_dir);
        Self {
            state: AppState {
                sessions,
                chaser,
                health: HealthState::new(),
            },
        }
    }

    pub fn create_router(&self) -> Router {
        Router::new()
            .route("/", get(index))
            .route("/health", get(health))
            .route("/v1", post(v1_handler))
            .with_state(self.state.clone())
    }
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

    let last_success_at = state
        .health
        .last_success_at
        .lock()
        .await
        .as_ref()
        .map(rfc3339);
    let last_failure_snapshot = state.health.last_failure.lock().await;
    let last_failure_at = last_failure_snapshot.as_ref().map(|f| rfc3339(&f.at));
    let last_failure_reason = last_failure_snapshot.as_ref().map(|f| f.reason.clone());
    drop(last_failure_snapshot);

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
    let mut ctx = LogContext::new()
        .with_operation(&req.cmd)
        .with_session(req.session.as_deref().unwrap_or("default"));

    if let Some(ref url) = req.url {
        ctx = ctx.with_url(url);
    }

    ctx.info("Incoming request");

    let mut response = match req.cmd.as_str() {
        "request.get" => handle_get(req, sm, &state.health).await,
        "request.post" => handle_post(req).await,
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
        ctx.info(&format!("Completed in {:.2}s", duration));
    } else {
        ctx.error(&format!(
            "Failed after {:.2}s: {}",
            duration, response.message
        ));
    }

    ResponseJson(response)
}

async fn handle_get(req: V1Request, sm: &SessionManager, health: &HealthState) -> V1Response {
    let url = match req.url {
        Some(ref u) => u.clone(),
        None => return V1Response::error("Parameter 'url' is mandatory for request.get"),
    };
    if req.post_data.is_some() {
        return V1Response::error("Cannot use 'postData' with GET request");
    }

    warn_deprecated(&req);

    // The upstream proxy is fixed at startup; per-request overrides are
    // warn-and-ignored to keep the wire format FlareSolverr-compatible.
    if req.proxy.is_some() {
        warn!("Per-request 'proxy' is ignored; upstream proxy is configured globally via env");
    }
    if req.session_ttl_minutes.is_some() {
        warn!("'session_ttl_minutes' on request.get is ignored; set it on sessions.create");
    }
    if req.max_timeout.is_some() {
        // chaser-cf enforces its own per-call timeout from CHASER_TIMEOUT;
        // the request-level value isn't plumbed through.
        debug!("'maxTimeout' is honored by chaser-cf via CHASER_TIMEOUT, not per-request");
    }

    let session_id = sm.resolve_session_id(req.session.as_deref());
    let is_default = session_id == DEFAULT_SESSION_ID;

    let handle = match sm.get_session(&session_id).await {
        Some(h) => h,
        None => return V1Response::error(&format!("Session not found: {}", session_id)),
    };

    // Holding this for the full request serializes concurrent calls on
    // the same session id so cookie/UA state can't be clobbered.
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

    let return_only_cookies = req.return_only_cookies.unwrap_or(false);
    let fetch_result = session.fetch(&url, return_only_cookies).await;

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

async fn handle_post(req: V1Request) -> V1Response {
    if req.post_data.is_none() {
        return V1Response::error("Parameter 'postData' is mandatory for request.post");
    }
    warn_deprecated(&req);
    V1Response::error(
        "request.post is not implemented by chaser-resolverr-rs; \
         configure your indexer to use GET or route POSTs through a different solver",
    )
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
