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
use std::time::Instant;
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
    let started = Instant::now();

    if needs_refresh(state, cfg, host.as_deref()) {
        // waf-session polls indefinitely for a cf_clearance cookie. On a
        // non-CF URL (e.g. Prowlarr's /v1/ping connectivity test) that
        // cookie never appears, so without a tight cap on this call we'd
        // burn the entire `maxTimeout` and fail the request. Cap at 1/3
        // of the budget — a real CF solve usually takes 5-10s anyway.
        let waf_timeout = (timeout / 3).max(5);
        debug!(
            "Refreshing chaser-cf waf-session for {} (cap {}s)",
            url, waf_timeout
        );
        match chaser.waf_session(url, proxy, waf_timeout).await {
            Ok(session) => apply_waf_session(state, &session, url),
            Err(e) => debug!(
                "waf-session for {} did not return clearance ({}); proceeding without cookie refresh",
                url, e
            ),
        }
        // Record the attempt either way — for non-CF hosts this stops us
        // retrying waf-session on every request until the TTL expires.
        state.clearance_fetched_at = Some(Utc::now().timestamp());
        state.clearance_host = host;
    } else {
        debug!("Reusing cached chaser-cf clearance for {}", url);
    }

    let body = if return_only_cookies {
        String::new()
    } else {
        let source_budget = timeout.saturating_sub(started.elapsed().as_secs()).max(5);
        chaser.source(url, proxy, source_budget).await?
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
