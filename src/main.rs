use anyhow::Result;
use log::{error, info};

mod chaser;
mod config;
mod fetcher;
mod flaresolverr;
mod logging;
mod session;

use flaresolverr::FlareSolverrAPI;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .format_timestamp_secs()
        .format_module_path(false)
        .format_target(false)
        .init();

    let config = config::load_from_env()?;

    let addr = config.bind_address();
    info!("FlareSolverr API server starting on {}", addr);
    info!("Session data directory: {}", config.data_path);
    match &config.proxy {
        Some(p) => info!(
            "Upstream proxy: {}:{} (auth: {})",
            p.host,
            p.port,
            p.username.is_some()
        ),
        None => info!("No upstream proxy configured; chaser-cf will egress directly"),
    }

    let api = FlareSolverrAPI::new(config);
    let app = api.create_router();

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
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
