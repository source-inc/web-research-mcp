pub mod camofox;
pub mod firecrawl;
pub mod searxng;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("backend HTTP error: status {status}, body: {body}")]
    Http { status: u16, body: String },
    #[error("backend timeout after {0:?}")]
    Timeout(Duration),
    #[error("backend transport error: {0}")]
    Transport(String),
    #[error("malformed backend response: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BackendKind {
    Searxng,
    Firecrawl,
    Camofox,
}

impl BackendKind {
    pub fn provider_label(&self) -> &'static str {
        match self {
            BackendKind::Searxng => "searxng",
            BackendKind::Firecrawl => "firecrawl-selfhosted",
            BackendKind::Camofox => "camofox",
        }
    }
}

pub fn build_http_client(timeout: Duration) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(concat!("web-research-mcp/", env!("CARGO_PKG_VERSION")))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_label_is_stable() {
        assert_eq!(BackendKind::Searxng.provider_label(), "searxng");
        assert_eq!(
            BackendKind::Firecrawl.provider_label(),
            "firecrawl-selfhosted"
        );
        assert_eq!(BackendKind::Camofox.provider_label(), "camofox");
    }

    #[test]
    fn http_client_builds() {
        let client = build_http_client(Duration::from_secs(5)).unwrap();
        let _ = client;
    }
}
