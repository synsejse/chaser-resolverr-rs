//! Simple logging utilities.

use log::{error, info};

/// Logging context with optional metadata. Methods emit a single line with
/// a grep-friendly `[op=… session=… url=…]` prefix.
#[derive(Clone, Debug, Default)]
pub struct LogContext {
    pub session_id: Option<String>,
    pub url: Option<String>,
    pub operation: Option<String>,
}

impl LogContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_session(mut self, session_id: &str) -> Self {
        self.session_id = Some(session_id.to_string());
        self
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    pub fn with_operation(mut self, operation: &str) -> Self {
        self.operation = Some(operation.to_string());
        self
    }

    fn prefix(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref op) = self.operation {
            parts.push(format!("op={}", op));
        }
        if let Some(ref sid) = self.session_id {
            parts.push(format!("session={}", sid));
        }
        if let Some(ref url) = self.url {
            let display = if url.len() > 60 { &url[..57] } else { url };
            parts.push(format!("url={}", display));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("[{}] ", parts.join(" "))
        }
    }

    pub fn info(&self, msg: &str) {
        info!("{}{}", self.prefix(), msg);
    }

    pub fn error(&self, msg: &str) {
        error!("{}{}", self.prefix(), msg);
    }
}
