use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HOST};
use reqwest::StatusCode;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use web_research_mcp::backends::camofox::CamofoxClient;
use web_research_mcp::backends::firecrawl::FirecrawlClient;
use web_research_mcp::backends::searxng::SearxngClient;
use web_research_mcp::config::{Domains, Families, Limits};
use web_research_mcp::mcp::WebResearchMcp;
use web_research_mcp::metrics::Metrics;
use web_research_mcp::policy::Policy;
use web_research_mcp::sessions::SessionRegistry;
use web_research_mcp::store::Store;

#[tokio::test]
async fn mcp_accepts_non_loopback_host_headers() -> anyhow::Result<()> {
    let cancel = CancellationToken::new();
    let app = test_server()?.into_http_router("/mcp", cancel.clone());
    let addr = spawn_server(app).await?;
    let client = reqwest::Client::new();
    let endpoint = format!("http://{addr}/mcp");

    let initialize = client
        .post(&endpoint)
        .header(HOST, "research-gateway.example.test:9213")
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "web-research-mcp-test",
                    "version": "0"
                }
            }
        }))
        .send()
        .await?;

    assert_eq!(initialize.status(), StatusCode::OK);
    let session_id = initialize
        .headers()
        .get("mcp-session-id")
        .expect("initialize response includes an MCP session id")
        .to_str()?
        .to_string();

    let initialized = client
        .post(&endpoint)
        .header(HOST, "research-gateway.example.test:9213")
        .header("mcp-session-id", &session_id)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .send()
        .await?;
    assert_eq!(initialized.status(), StatusCode::ACCEPTED);

    let tools = client
        .post(&endpoint)
        .header(HOST, "research-gateway.example.test:9213")
        .header("mcp-session-id", session_id)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }))
        .send()
        .await?;

    assert_eq!(tools.status(), StatusCode::OK);
    let body = tools.text().await?;
    assert!(body.contains("\"web_collect_evidence\""));
    assert!(body.contains("\"web_search\""));
    assert!(body.contains("\"web_scrape_url\""));
    assert!(body.contains("\"browser_open\""));

    cancel.cancel();
    Ok(())
}

fn test_server() -> anyhow::Result<WebResearchMcp> {
    let timeout = Duration::from_millis(100);
    Ok(WebResearchMcp::new(
        Policy::new(Families::default(), Limits::default(), Domains::default()),
        Store::noop(),
        SessionRegistry::new(4),
        Metrics::new()?,
        false,
        SearxngClient::new("http://127.0.0.1:1".into(), timeout)?,
        FirecrawlClient::new("http://127.0.0.1:1".into(), None, timeout)?,
        CamofoxClient::new("http://127.0.0.1:1".into(), None, timeout)?,
    ))
}

async fn spawn_server(app: Router) -> anyhow::Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}
