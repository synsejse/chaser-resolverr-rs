//! Thin adapter around the vendored `chaser_cf` library — translates our
//! config + cookie types into chaser-cf's types and back.

use anyhow::{Result, anyhow};
use chaser_cf::{ChaserCF, ChaserConfig, ProxyConfig as CfProxy, SourceResponse, WafSession};
use std::collections::HashMap;
use std::sync::Arc;

use crate::config::ProxyConfig;

/// Wraps an `Arc<ChaserCF>` so the browser stays alive across all sessions
/// and clones are cheap. Initializes lazily on first request.
#[derive(Clone)]
pub struct ChaserClient {
    inner: Arc<ChaserCF>,
}

impl ChaserClient {
    pub async fn init() -> Result<Self> {
        let config = ChaserConfig::from_env();
        let inner = ChaserCF::new(config)
            .await
            .map_err(|e| anyhow!("chaser-cf init failed: {}", e))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }

    pub async fn is_ready(&self) -> bool {
        self.inner.is_ready().await
    }

    pub async fn waf_session(&self, url: &str, proxy: Option<&ProxyConfig>) -> Result<WafSummary> {
        let session: WafSession = self
            .inner
            .solve_waf_session(url, proxy.map(CfProxy::from))
            .await
            .map_err(|e| anyhow!("waf-session failed: {}", e))?;
        Ok(WafSummary {
            cookies: session
                .cookies
                .into_iter()
                .map(|c| WafCookie {
                    name: c.name,
                    value: c.value,
                    domain: c.domain,
                    path: c.path,
                    expires: c.expires,
                    http_only: c.http_only,
                    secure: c.secure,
                    same_site: c.same_site,
                })
                .collect(),
            headers: session.headers,
        })
    }

    pub async fn source(&self, url: &str, proxy: Option<&ProxyConfig>) -> Result<SourceResponse> {
        self.inner
            .get_source(url, proxy.map(CfProxy::from))
            .await
            .map_err(|e| anyhow!("source failed: {}", e))
    }

    pub async fn post(
        &self,
        url: &str,
        post_data: &str,
        proxy: Option<&ProxyConfig>,
    ) -> Result<SourceResponse> {
        self.inner
            .post_source(url, post_data, proxy.map(CfProxy::from))
            .await
            .map_err(|e| anyhow!("post failed: {}", e))
    }
}

impl From<&ProxyConfig> for CfProxy {
    fn from(p: &ProxyConfig) -> Self {
        let mut cfp = CfProxy::new(p.host.clone(), p.port);
        if let (Some(u), Some(pw)) = (&p.username, &p.password) {
            cfp = cfp.with_auth(u.clone(), pw.clone());
        }
        cfp
    }
}

/// Trimmed waf-session payload used by the fetcher (and not exposing the
/// upstream `Cookie`/`WafSession` types beyond this module).
pub struct WafSummary {
    pub cookies: Vec<WafCookie>,
    pub headers: HashMap<String, String>,
}

impl WafSummary {
    /// Case-insensitive lookup — both `User-Agent` and `user-agent` have
    /// shipped at various points.
    pub fn user_agent(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.as_str())
    }
}

pub struct WafCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Fractional seconds since the Unix epoch.
    pub expires: Option<f64>,
    pub http_only: Option<bool>,
    pub secure: Option<bool>,
    pub same_site: Option<String>,
}
