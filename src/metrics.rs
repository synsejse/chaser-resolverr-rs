//! Prometheus metrics wiring.
//!
//! A single global recorder is installed at startup; counters are emitted
//! from the request path and rendered on demand at `/metrics`. No HTTP
//! listener is spawned by the metrics crate — the axum app serves the
//! endpoint so it shares the same port and lifecycle as everything else.

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

/// Counter: every fetch attempt (GET or POST), success or failure.
pub const FETCHES_TOTAL: &str = "chaser_fetches_total";
/// Counter: fetch attempts that ended in an error.
pub const FETCHES_FAILED: &str = "chaser_fetches_failed";

/// Install the global Prometheus recorder and return a handle for rendering.
pub fn install_recorder() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new().install_recorder()?;
    metrics::describe_counter!(FETCHES_TOTAL, "Total fetch attempts (GET + POST)");
    metrics::describe_counter!(FETCHES_FAILED, "Fetch attempts that ended in an error");
    Ok(handle)
}

/// Record the outcome of a single fetch. Mirrors the counters surfaced by
/// `/health` so the two never drift.
pub fn record_fetch(success: bool) {
    metrics::counter!(FETCHES_TOTAL).increment(1);
    if !success {
        metrics::counter!(FETCHES_FAILED).increment(1);
    }
}
