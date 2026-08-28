// crates/web-research-mcp/src/backends/firecrawl.rs
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{build_http_client, BackendError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScrapeResult {
    pub url: String,
    #[serde(default)]
    pub markdown: String,
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub status_code: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapEntry {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Clone)]
pub struct FirecrawlClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    timeout: Duration,
}

impl FirecrawlClient {
    pub fn new(
        endpoint: String,
        api_key: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            http: build_http_client(timeout)?,
            endpoint,
            api_key,
            timeout,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.endpoint.trim_end_matches('/'), path)
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    pub async fn scrape(&self, url: &str, rendered: bool) -> Result<ScrapeResult, BackendError> {
        let body = serde_json::json!({
            "url": url,
            "formats": ["markdown", "links"],
            "onlyMainContent": true,
            "waitFor": if rendered { 1000 } else { 0 },
        });
        let resp = self
            .auth(self.http.post(self.url("/v1/scrape")).json(&body))
            .send()
            .await
            .map_err(|error| transport_or_timeout(error, self.timeout))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let envelope: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        let data = envelope
            .get("data")
            .ok_or_else(|| BackendError::Malformed("missing data".into()))?;
        let markdown = data
            .get("markdown")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let links = data
            .get("links")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let metadata = data
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        // Firecrawl self-host returns HTTP 200 even when the fetch failed (DNS
        // failure, blocked target, upstream 4xx/5xx): it reports empty markdown
        // plus a `metadata.error` string and the target `statusCode`. Surface
        // that as a backend error so a failed fetch isn't presented as empty
        // "successful" evidence.
        if markdown.is_empty() {
            let status_code = metadata.get("statusCode").and_then(|v| v.as_u64());
            // Only treat as a failed fetch when the target status actually
            // indicates failure (or is absent) — a legitimate 2xx page that
            // simply has no main content is empty evidence, not an error, even
            // if metadata carries a non-fatal warning string.
            let failed_status = status_code.map(|c| c >= 400).unwrap_or(true);
            if failed_status {
                if let Some(err) = metadata
                    .get("error")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    return Err(BackendError::Http {
                        status: status_code.unwrap_or(502) as u16,
                        body: err.to_string(),
                    });
                }
            }
        }
        Ok(ScrapeResult {
            url: url.to_string(),
            markdown,
            links,
            metadata,
            status_code: None,
        })
    }

    pub async fn map(&self, url: &str, max_pages: usize) -> Result<Vec<MapEntry>, BackendError> {
        let body = serde_json::json!({ "url": url, "limit": max_pages });
        let resp = self
            .auth(self.http.post(self.url("/v1/map")).json(&body))
            .send()
            .await
            .map_err(|error| transport_or_timeout(error, self.timeout))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let envelope: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        let links = envelope
            .get("links")
            .and_then(|v| v.as_array())
            .ok_or_else(|| BackendError::Malformed("missing links".into()))?;
        Ok(links
            .iter()
            .filter_map(|v| {
                v.as_str().map(|s| MapEntry {
                    url: s.to_string(),
                    title: None,
                })
            })
            .collect())
    }

    pub async fn crawl(
        &self,
        url: &str,
        max_depth: usize,
        max_pages: usize,
    ) -> Result<Vec<ScrapeResult>, BackendError> {
        // Step 1: POST /v1/crawl to initiate the async job.
        let body = serde_json::json!({
            "url": url,
            "limit": max_pages,
            "maxDepth": max_depth,
            "scrapeOptions": { "formats": ["markdown", "links"], "onlyMainContent": true }
        });
        let resp = self
            .auth(self.http.post(self.url("/v1/crawl")).json(&body))
            .send()
            .await
            .map_err(|error| transport_or_timeout(error, self.timeout))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let start_envelope: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        let job_id = start_envelope
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                BackendError::Malformed("missing job id in crawl start response".into())
            })?
            .to_string();

        // Step 2: Poll GET /v1/crawl/{id} until completed or failed.
        let deadline = Instant::now() + Duration::from_secs(120);
        let poll_url = self.url(&format!("/v1/crawl/{job_id}"));
        let mut all_pages: Vec<ScrapeResult> = Vec::new();

        loop {
            if Instant::now() >= deadline {
                return Err(BackendError::Timeout(Duration::from_secs(120)));
            }

            let poll_resp = self
                .auth(self.http.get(&poll_url))
                .send()
                .await
                .map_err(|error| transport_or_timeout(error, self.timeout))?;
            let poll_status = poll_resp.status();
            if !poll_status.is_success() {
                return Err(BackendError::Http {
                    status: poll_status.as_u16(),
                    body: poll_resp.text().await.unwrap_or_default(),
                });
            }
            let poll_body: serde_json::Value = poll_resp
                .json()
                .await
                .map_err(|e| BackendError::Malformed(e.to_string()))?;
            let job_status = poll_body
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match job_status {
                "failed" => {
                    return Err(BackendError::Http {
                        status: 502,
                        body: "crawl job failed".into(),
                    });
                }
                "completed" => {
                    collect_pages(&poll_body, &mut all_pages);

                    // Step 3: Follow pagination via `next` URL if present.
                    let mut next_url = poll_body
                        .get("next")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(String::from);
                    while let Some(ref nurl) = next_url.clone() {
                        if Instant::now() >= deadline {
                            return Err(BackendError::Timeout(Duration::from_secs(120)));
                        }
                        let page_resp = self
                            .auth(self.http.get(nurl))
                            .send()
                            .await
                            .map_err(|error| transport_or_timeout(error, self.timeout))?;
                        if !page_resp.status().is_success() {
                            break;
                        }
                        let page_body: serde_json::Value = page_resp
                            .json()
                            .await
                            .map_err(|e| BackendError::Malformed(e.to_string()))?;
                        collect_pages(&page_body, &mut all_pages);
                        next_url = page_body
                            .get("next")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from);
                    }
                    return Ok(all_pages);
                }
                _ => {
                    // Still scraping; wait and retry.
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
}

/// Extract page objects from a crawl poll response and append to `out`.
fn collect_pages(body: &serde_json::Value, out: &mut Vec<ScrapeResult>) {
    if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
        for item in arr {
            let url = item
                .get("metadata")
                .and_then(|m| m.get("sourceURL"))
                .and_then(|v| v.as_str())
                .or_else(|| item.get("url").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string();
            let markdown = item
                .get("markdown")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let links = item
                .get("links")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let metadata = item
                .get("metadata")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            out.push(ScrapeResult {
                url,
                markdown,
                links,
                metadata,
                status_code: None,
            });
        }
    }
}

fn transport_or_timeout(e: reqwest::Error, timeout: Duration) -> BackendError {
    if e.is_timeout() {
        BackendError::Timeout(timeout)
    } else {
        BackendError::Transport(e.to_string())
    }
}
