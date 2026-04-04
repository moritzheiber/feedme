use clap::Parser;
use feedme::{api, cli, config, db, fetcher};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = cli::Cli::parse();

    let config = config::Config::from_env().map_err(|e| e.to_string())?;
    let pool = db::init_pool(&config.database_url).await?;

    match args.command {
        cli::Command::Serve { host, port } => {
            let bind_host = host.unwrap_or(config.host);
            let bind_port = port.unwrap_or(config.port);
            let addr = format!("{}:{}", bind_host, bind_port);

            let state = api::AppState::new(pool.clone(), config.api_key);
            let app = api::router(state);

            let shutdown_token = CancellationToken::new();

            let client = fetcher::build_client().map_err(|e| e.to_string())?;
            let scheduler_pool = pool.clone();
            let scheduler_token = shutdown_token.clone();

            let scheduler_handle = tokio::spawn(async move {
                fetcher::run_scheduler(scheduler_pool, client, scheduler_token).await;
            });

            let listener = tokio::net::TcpListener::bind(&addr).await?;
            tracing::info!(addr = %addr, "server listening");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal())
                .await?;

            tracing::info!("server stopped, shutting down scheduler");
            shutdown_token.cancel();
            let _ = scheduler_handle.await;
        }
        cli::Command::Feed { action } => {
            cli::handle_feed_action(&pool, action).await?;
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");

    tokio::select! {
        _ = sigint.recv() => tracing::info!("received SIGINT"),
        _ = sigterm.recv() => tracing::info!("received SIGTERM"),
    }
}
