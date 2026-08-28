use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use web_research_mcp::{
    backends::{camofox::CamofoxClient, firecrawl::FirecrawlClient, searxng::SearxngClient},
    config::{Cli, Command, Config},
    health::{router as health_router, HealthState},
    mcp::WebResearchMcp,
    metrics::Metrics,
    policy::Policy,
    sessions::SessionRegistry,
    store::Store,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if matches!(cli.command, Command::Version) {
        println!("web-research-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = cli.config.clone().or_else(|| {
        let default = Config::default_config_path();
        default.exists().then_some(default)
    });

    let mut config = Config::load(config_path.as_deref())?;
    config.apply_env_overrides();

    match cli.command {
        Command::Version => unreachable!(),
        Command::Serve => serve(config).await,
        Command::Smoke { backend } => smoke(config, &backend).await,
    }
}

async fn serve(config: Config) -> Result<()> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating {}", config.data_dir.display()))?;

    let store = Store::disk(&config.data_dir).await?;

    let metrics = Metrics::new()?;
    let policy = Policy::new(
        config.policy.families.clone(),
        config.policy.limits.clone(),
        config.policy.domains.clone(),
    );
    let sessions = SessionRegistry::new(config.policy.limits.browser_max_concurrent);

    let timeout_scrape = std::time::Duration::from_secs(30);
    let timeout_browser = std::time::Duration::from_secs(60);

    let searxng = SearxngClient::new(config.backends.searxng.endpoint.clone(), timeout_scrape)?;
    let firecrawl = FirecrawlClient::new(
        config.backends.firecrawl.endpoint.clone(),
        config.backends.firecrawl.api_key.clone(),
        timeout_scrape,
    )?;
    let camofox = CamofoxClient::new(
        config.backends.camofox.endpoint.clone(),
        config.backends.camofox.api_key.clone(),
        timeout_browser,
    )?;

    let mcp = WebResearchMcp::new_with_exposed_tools(
        policy,
        store,
        sessions,
        metrics.clone(),
        config.policy.browser.cookie_import_enabled,
        searxng,
        firecrawl,
        camofox,
        config.exposed_tools.as_deref(),
    )?;

    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".into());
    let health_state = Arc::new(HealthState { host, metrics });
    let cancel = CancellationToken::new();

    let bind_addr: IpAddr = config
        .http_bind_addr
        .parse()
        .with_context(|| format!("invalid HTTP bind address '{}'", config.http_bind_addr))?;
    let addr = SocketAddr::new(bind_addr, config.http_port);

    let app =
        health_router(health_state).merge(mcp.into_http_router(&config.mcp_path, cancel.clone()));

    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| {
        format!(
            "binding web-research-mcp HTTP server to {}:{}",
            config.http_bind_addr, config.http_port
        )
    })?;
    let local_addr = listener
        .local_addr()
        .context("reading HTTP listen address")?;

    tracing::info!(
        bind = %local_addr,
        mcp_path = %config.mcp_path,
        "web-research-mcp ready"
    );

    let server_cancel = cancel.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { server_cancel.cancelled_owned().await })
        .await
        .context("serving web-research-mcp HTTP")?;

    Ok(())
}

async fn smoke(config: Config, backend: &str) -> Result<()> {
    match backend {
        "searxng" => {
            let c = SearxngClient::new(
                config.backends.searxng.endpoint.clone(),
                std::time::Duration::from_secs(10),
            )?;
            let results = c.search("rust", 3, None, None, None).await?;
            println!("searxng OK: {} results", results.len());
        }
        "firecrawl" => {
            let c = FirecrawlClient::new(
                config.backends.firecrawl.endpoint.clone(),
                config.backends.firecrawl.api_key.clone(),
                std::time::Duration::from_secs(30),
            )?;
            let r = c.scrape("https://example.com", false).await?;
            println!("firecrawl OK: {} bytes markdown", r.markdown.len());
        }
        "camofox" => {
            let c = CamofoxClient::new(
                config.backends.camofox.endpoint.clone(),
                config.backends.camofox.api_key.clone(),
                std::time::Duration::from_secs(60),
            )?;
            let s = c.open(Some("https://example.com")).await?;
            let snap = c.snapshot(&s.session_id).await?;
            let _ = c.close(&s.session_id).await;
            println!("camofox OK: snapshot {} chars", snap.text.len());
        }
        other => anyhow::bail!("unknown backend: {other}"),
    }
    Ok(())
}
