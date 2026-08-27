use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{build_http_client, BackendError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearxResult {
    pub url: String,
    pub title: String,
    #[serde(default, rename = "content")]
    pub snippet: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default, rename = "publishedDate", alias = "published_date")]
    pub published_at: Option<String>,
}

#[derive(Clone)]
pub struct SearxngClient {
    http: reqwest::Client,
    endpoint: String,
}

impl SearxngClient {
    pub fn new(endpoint: String, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            http: build_http_client(timeout)?,
            endpoint,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        categories: Option<&str>,
        language: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<Vec<SearxResult>, BackendError> {
        let mut req = self
            .http
            .get(format!("{}/search", self.endpoint.trim_end_matches('/')))
            .query(&[("q", query), ("format", "json")]);
        if let Some(cats) = categories {
            req = req.query(&[("categories", cats)]);
        }
        if let Some(language) = language {
            req = req.query(&[("language", language)]);
        }
        if let Some(time_range) = time_range {
            req = req.query(&[("time_range", time_range)]);
        }

        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                BackendError::Timeout(Duration::from_secs(0))
            } else {
                BackendError::Transport(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(BackendError::Http {
                status: status.as_u16(),
                body,
            });
        }

        let body: Value = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        let results = body
            .get("results")
            .and_then(|v| v.as_array())
            .ok_or_else(|| BackendError::Malformed("missing results array".to_string()))?;

        let parsed: Vec<SearxResult> = results
            .iter()
            .take(max_results)
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        Ok(parsed)
    }
}
