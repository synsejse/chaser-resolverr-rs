use anyhow::{Context, Result};
use tracing::{error, info};

mod chaser;
mod config;
mod fetcher;
mod flaresolverr;
mod metrics;
mod session;

use chaser::ChaserClient;
use flaresolverr::FlareSolverrAPI;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let prometheus = metrics::install_recorder().context("install Prometheus recorder")?;

    let config = config::load_from_env()?;

    info!("Initializing chaser-cf browser...");
    let chaser = ChaserClient::init().await?;
    info!("chaser-cf browser ready");

    match &config.proxy {
        Some(p) => info!(
            "Upstream proxy: {}:{} (auth: {})",
            p.host,
            p.port,
            p.username.is_some()
        ),
        None => info!("No upstream proxy configured; chaser-cf will egress directly"),
    }

    let (browser_cfg, runtime) = config.into_browser_config(chaser.clone());
    info!("FlareSolverr API server starting on {}", runtime.bind);
    info!("Session data directory: {}", runtime.data_path);

    let api = FlareSolverrAPI::new(browser_cfg, &runtime.data_path, prometheus);
    let app = api.create_router();

    let listener = tokio::net::TcpListener::bind(&runtime.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("Shutting down chaser-cf...");
    chaser.shutdown().await;
    Ok(())
}

/// Initialize the tracing subscriber. Honors `RUST_LOG` (e.g.
/// `RUST_LOG=debug` or `chaser_resolverr_rs=debug,chaser_cf=info`); defaults
/// to `info`. Captures both this crate's events and the vendored chaser-cf's
/// `tracing` output through a single subscriber.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt().with_env_filter(filter).with_target(false).init();
}

async fn shutdown_signal() {
    use tokio::signal;

    let ctrl_c = signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(e) => {
                error!("Failed to register SIGTERM handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = async { std::future::pending::<()>().await };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("Shutdown signal received, shutting down...");
}
