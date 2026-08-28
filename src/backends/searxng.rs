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
    pub engines: Vec<String>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default, rename = "publishedDate", alias = "published_date")]
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearxEngineError {
    pub engine: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearxSearchResponse {
    pub results: Vec<SearxResult>,
    pub unresponsive_engines: Vec<SearxEngineError>,
}

#[derive(Clone)]
pub struct SearxngClient {
    http: reqwest::Client,
    endpoint: String,
    timeout: Duration,
}

impl SearxngClient {
    pub fn new(endpoint: String, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            http: build_http_client(timeout)?,
            endpoint,
            timeout,
        })
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        categories: Option<&str>,
        language: Option<&str>,
        time_range: Option<&str>,
    ) -> Result<SearxSearchResponse, BackendError> {
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
                BackendError::Timeout(self.timeout)
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
        let unresponsive_engines = body
            .get("unresponsive_engines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let pair = entry.as_array()?;
                Some(SearxEngineError {
                    engine: pair.first()?.as_str()?.to_string(),
                    reason: pair.get(1)?.as_str()?.to_string(),
                })
            })
            .collect();
        Ok(SearxSearchResponse {
            results: parsed,
            unresponsive_engines,
        })
    }
}
