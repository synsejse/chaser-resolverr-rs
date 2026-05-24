//! Fetch pipeline.
//!
//! `cf_clearance` + UA come from `chaser.waf_session(url)` and are cached
//! on `SessionData` for `clearance_ttl_seconds`. The body always comes
//! from a fresh `chaser.source(url)` — fetching directly with reqwest
//! and a borrowed clearance fails routinely because Cloudflare also
//! fingerprints TLS/HTTP at the connection layer.

use anyhow::Result;
use chrono::Utc;
use log::debug;
use url::Url;

use crate::chaser::{ChaserClient, WafSession};
use crate::config::{ProxyConfig, SessionConfig};
use crate::session::{SessionData, StoredCookie};

pub struct FetchResponse {
    pub url: String,
    pub status: u16,
    pub body: String,
    pub cookies: Vec<StoredCookie>,
    pub user_agent: String,
}

/// chaser-cf's `source` mode doesn't surface the upstream HTTP status,
/// so we report 200 on a successful body fetch.
const DEFAULT_HTTP_STATUS: u16 = 200;

pub async fn fetch(
    chaser: &ChaserClient,
    proxy: Option<&ProxyConfig>,
    cfg: &SessionConfig,
    state: &mut SessionData,
    url: &str,
    timeout: u64,
    return_only_cookies: bool,
) -> Result<FetchResponse> {
    let host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from));

    if needs_refresh(state, cfg, host.as_deref()) {
        debug!("Refreshing chaser-cf waf-session for {}", url);
        let session = chaser.waf_session(url, proxy, timeout).await?;
        apply_waf_session(state, &session, url);
        state.clearance_fetched_at = Some(Utc::now().timestamp());
        state.clearance_host = host;
    } else {
        debug!("Reusing cached chaser-cf clearance for {}", url);
    }

    let body = if return_only_cookies {
        String::new()
    } else {
        chaser.source(url, proxy, timeout).await?
    };

    Ok(FetchResponse {
        url: url.to_string(),
        status: DEFAULT_HTTP_STATUS,
        body,
        cookies: state.cookies.clone(),
        user_agent: state.user_agent.clone(),
    })
}

fn needs_refresh(state: &SessionData, cfg: &SessionConfig, host: Option<&str>) -> bool {
    if state.clearance_host.as_deref() != host {
        return true;
    }
    let Some(fetched_at) = state.clearance_fetched_at else {
        return true;
    };
    let age = Utc::now().timestamp() - fetched_at;
    age < 0 || age >= cfg.clearance_ttl_seconds
}

fn apply_waf_session(state: &mut SessionData, session: &WafSession, url: &str) {
    let fallback_host = Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(String::from));

    if let Some(ua) = session.user_agent() {
        state.user_agent = ua.to_string();
    }

    for incoming in &session.cookies {
        let cookie = StoredCookie {
            name: incoming.name.clone(),
            value: incoming.value.clone(),
            domain: incoming.domain.clone().or_else(|| fallback_host.clone()),
            path: incoming.path.clone().or_else(|| Some("/".to_string())),
            expires: incoming.expires.map(|e| e as i64),
            http_only: incoming.http_only.unwrap_or(false),
            secure: incoming.secure.unwrap_or(false),
            same_site: incoming.same_site.clone(),
        };
        upsert_cookie(&mut state.cookies, cookie);
    }
}

/// Upsert on (name, domain, path) — Chrome-style cookie identity, so a
/// newer cookie replaces an existing one rather than creating a duplicate
/// the browser would later pick from at random.
pub fn upsert_cookie(cookies: &mut Vec<StoredCookie>, incoming: StoredCookie) {
    if let Some(existing) = cookies
        .iter_mut()
        .find(|c| c.name == incoming.name && c.domain == incoming.domain && c.path == incoming.path)
    {
        *existing = incoming;
    } else {
        cookies.push(incoming);
    }
}

pub fn purge_expired(cookies: &mut Vec<StoredCookie>) {
    let now = Utc::now().timestamp();
    cookies.retain(|c| c.expires.is_none_or(|e| e > now));
}
