//! chaser-cf HTTP client (`POST /solve`).
//!
//! The upstream README documents an enveloped shape
//! (`{"type":"WafSession","data":{…}}`) but the deployed server actually
//! returns a flat object — `{"code":200,"cookies":[…],"headers":{…}}` for
//! `waf-session`, `{"code":200,"source":"<html>"}` for `source`,
//! `{"code":<non-200>,"message":"…"}` on error. We parse the flat form.

use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::config::ProxyConfig;

const SUCCESS_CODE: i64 = 200;

#[derive(Debug, Clone)]
pub struct ChaserClient {
    client: Client,
    endpoint: String,
    auth_token: Option<String>,
}

impl Default for ChaserClient {
    fn default() -> Self {
        Self::new("http://127.0.0.1:3000".to_string(), None)
    }
}

impl ChaserClient {
    pub fn new(endpoint: String, auth_token: Option<String>) -> Self {
        Self {
            client: Client::new(),
            endpoint,
            auth_token,
        }
    }

    /// Any HTTP response from the root URL (even 404 / 405) counts as alive
    /// — chaser-cf doesn't expose a dedicated health endpoint.
    pub async fn ping(&self, timeout: u64) -> Result<()> {
        let probe = Client::builder()
            .timeout(Duration::from_secs(timeout))
            .build()
            .map_err(|e| anyhow!("client build failed: {}", e))?;
        probe
            .get(self.endpoint.trim_end_matches('/'))
            .send()
            .await
            .map(|_| ())
            .map_err(|e| anyhow!("connect: {}", e))
    }

    pub async fn waf_session(
        &self,
        url: &str,
        proxy: Option<&ProxyConfig>,
        timeout: u64,
    ) -> Result<WafSession> {
        let resp: SolveResponse = self.solve("waf-session", url, proxy, None, timeout).await?;
        if resp.code != SUCCESS_CODE {
            return Err(anyhow!(
                "chaser-cf error code={} message={:?}",
                resp.code,
                resp.message.unwrap_or_default()
            ));
        }
        Ok(WafSession {
            cookies: resp.cookies.unwrap_or_default(),
            headers: resp.headers.unwrap_or_default(),
        })
    }

    pub async fn source(
        &self,
        url: &str,
        proxy: Option<&ProxyConfig>,
        timeout: u64,
    ) -> Result<String> {
        let resp: SolveResponse = self.solve("source", url, proxy, None, timeout).await?;
        if resp.code != SUCCESS_CODE {
            return Err(anyhow!(
                "chaser-cf error code={} message={:?}",
                resp.code,
                resp.message.unwrap_or_default()
            ));
        }
        resp.source
            .ok_or_else(|| anyhow!("chaser-cf source response missing `source` field"))
    }

    async fn solve(
        &self,
        mode: &str,
        url: &str,
        proxy: Option<&ProxyConfig>,
        site_key: Option<&str>,
        timeout: u64,
    ) -> Result<SolveResponse> {
        let req = SolveRequest {
            mode,
            url,
            proxy: proxy.map(ChaserProxy::from),
            site_key,
        };

        let mut builder = self
            .client
            .post(format!("{}/solve", self.endpoint.trim_end_matches('/')))
            .json(&req)
            .timeout(Duration::from_secs(timeout));

        if let Some(token) = &self.auth_token {
            builder = builder.bearer_auth(token);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| anyhow!("chaser-cf request failed: {}", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow!("chaser-cf body read failed: {}", e))?;

        if !status.is_success() {
            return Err(anyhow!("chaser-cf returned HTTP {}: {}", status, body));
        }

        serde_json::from_str(&body)
            .map_err(|e| anyhow!("Failed to parse chaser-cf response ({}): {}", e, body))
    }
}

#[derive(Debug, Serialize)]
struct SolveRequest<'a> {
    mode: &'a str,
    url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy: Option<ChaserProxy>,
    #[serde(rename = "siteKey", skip_serializing_if = "Option::is_none")]
    site_key: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct ChaserProxy {
    host: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

impl From<&ProxyConfig> for ChaserProxy {
    fn from(p: &ProxyConfig) -> Self {
        Self {
            host: p.host.clone(),
            port: p.port,
            username: p.username.clone(),
            password: p.password.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SolveResponse {
    code: i64,
    #[serde(default)]
    cookies: Option<Vec<WafCookie>>,
    #[serde(default)]
    headers: Option<HashMap<String, String>>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WafSession {
    pub cookies: Vec<WafCookie>,
    pub headers: HashMap<String, String>,
}

impl WafSession {
    /// Case-insensitive lookup — chaser-cf has shipped both `User-Agent`
    /// and `user-agent` at different points.
    pub fn user_agent(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct WafCookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Fractional seconds since the Unix epoch.
    #[serde(default)]
    pub expires: Option<f64>,
    #[serde(default)]
    pub http_only: Option<bool>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default)]
    pub same_site: Option<String>,
}
