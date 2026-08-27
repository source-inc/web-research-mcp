use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{build_http_client, BackendError};

const USER_ID: &str = "web-research-mcp";

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// Returned by `open()`. The `session_id` field exposes the internal `tabId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenResult {
    pub session_id: String,
}

/// Raw shape of `POST /tabs/open` response.
#[derive(Debug, Clone, Deserialize)]
struct TabOpenRaw {
    #[serde(rename = "tabId")]
    tab_id: String,
}

/// Returned by `snapshot()` and action-then-snapshot helpers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResult {
    pub url: String,
    #[serde(rename = "snapshot")]
    pub text: String,
}

/// Returned by `screenshot()` — the base64 data and MIME type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub data: String,
    pub mime_type: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct CamofoxClient {
    http: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
}

impl CamofoxClient {
    pub fn new(
        endpoint: String,
        api_key: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            http: build_http_client(timeout)?,
            endpoint,
            api_key,
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

    /// Ensure the browser process is started. Idempotent — errors are silently
    /// ignored since the endpoint returns 200 even when already running.
    async fn ensure_started(&self) {
        let _ = self.auth(self.http.post(self.url("/start"))).send().await;
    }

    pub async fn open(&self, start_url: Option<&str>) -> Result<OpenResult, BackendError> {
        let url = start_url.unwrap_or("about:blank");

        // Ensure the browser is running before opening a tab.
        self.ensure_started().await;

        let tab_id = self.open_tab(url).await?;
        Ok(OpenResult { session_id: tab_id })
    }

    /// Internal: POST /tabs/open.  On `session_expired`, call /start and retry once.
    async fn open_tab(&self, url: &str) -> Result<String, BackendError> {
        let body = serde_json::json!({ "userId": USER_ID, "url": url });
        let resp = self
            .auth(self.http.post(self.url("/tabs/open")).json(&body))
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            let raw = resp.text().await.unwrap_or_default();
            // Check for session_expired and retry after /start.
            if raw.contains("session_expired") {
                self.ensure_started().await;
                return self.open_tab_once(url).await;
            }
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: raw,
            });
        }
        let raw: TabOpenRaw = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        Ok(raw.tab_id)
    }

    /// One-shot tab open without retry.
    async fn open_tab_once(&self, url: &str) -> Result<String, BackendError> {
        let body = serde_json::json!({ "userId": USER_ID, "url": url });
        let resp = self
            .auth(self.http.post(self.url("/tabs/open")).json(&body))
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        let raw: TabOpenRaw = resp
            .json()
            .await
            .map_err(|e| BackendError::Malformed(e.to_string()))?;
        Ok(raw.tab_id)
    }

    pub async fn close(&self, tab_id: &str) -> Result<(), BackendError> {
        let resp = self
            .auth(
                self.http
                    .delete(self.url(&format!("/tabs/{tab_id}")))
                    .query(&[("userId", USER_ID)]),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        if !resp.status().is_success() {
            return Err(BackendError::Http {
                status: resp.status().as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        Ok(())
    }

    pub async fn snapshot(&self, tab_id: &str) -> Result<SnapshotResult, BackendError> {
        let resp = self
            .auth(
                self.http
                    .get(self.url(&format!("/tabs/{tab_id}/snapshot")))
                    .query(&[("userId", USER_ID)]),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        json_or_err(resp).await
    }

    pub async fn screenshot(
        &self,
        tab_id: &str,
        full_page: bool,
    ) -> Result<ScreenshotResult, BackendError> {
        let mut query = vec![("userId", USER_ID.to_string())];
        if full_page {
            query.push(("fullPage", "true".to_string()));
        }
        let resp = self
            .auth(
                self.http
                    .get(self.url(&format!("/tabs/{tab_id}/screenshot")))
                    .query(&query),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        // The live server returns the raw image bytes (content-type image/png),
        // not the JSON shape the OpenAPI advertises — base64-encode them.
        let mime_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("image/png")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| BackendError::Transport(e.to_string()))?;
        let data = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        Ok(ScreenshotResult { data, mime_type })
    }

    pub async fn click(&self, tab_id: &str, target: &str) -> Result<SnapshotResult, BackendError> {
        let body = serde_json::json!({ "userId": USER_ID, "ref": target });
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/tabs/{tab_id}/click")))
                    .json(&body),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        // Click does not return a snapshot; fetch one immediately.
        self.snapshot(tab_id).await
    }

    pub async fn type_text(
        &self,
        tab_id: &str,
        target: &str,
        text: &str,
    ) -> Result<SnapshotResult, BackendError> {
        let body = serde_json::json!({ "userId": USER_ID, "ref": target, "text": text });
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/tabs/{tab_id}/type")))
                    .json(&body),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        self.snapshot(tab_id).await
    }

    pub async fn scroll(
        &self,
        tab_id: &str,
        direction: &str,
        amount: Option<i32>,
    ) -> Result<SnapshotResult, BackendError> {
        // Omit `amount` entirely when not provided — the server 500s on `amount: null`.
        let mut body = serde_json::Map::new();
        body.insert("userId".into(), serde_json::json!(USER_ID));
        body.insert("direction".into(), serde_json::json!(direction));
        if let Some(a) = amount {
            body.insert("amount".into(), serde_json::json!(a));
        }
        let body = serde_json::Value::Object(body);
        let resp = self
            .auth(
                self.http
                    .post(self.url(&format!("/tabs/{tab_id}/scroll")))
                    .json(&body),
            )
            .send()
            .await
            .map_err(transport_or_timeout)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(BackendError::Http {
                status: status.as_u16(),
                body: resp.text().await.unwrap_or_default(),
            });
        }
        self.snapshot(tab_id).await
    }
}

fn transport_or_timeout(e: reqwest::Error) -> BackendError {
    if e.is_timeout() {
        BackendError::Timeout(Duration::from_secs(0))
    } else {
        BackendError::Transport(e.to_string())
    }
}

async fn json_or_err<T: for<'de> Deserialize<'de>>(
    resp: reqwest::Response,
) -> Result<T, BackendError> {
    let status = resp.status();
    if !status.is_success() {
        return Err(BackendError::Http {
            status: status.as_u16(),
            body: resp.text().await.unwrap_or_default(),
        });
    }
    resp.json()
        .await
        .map_err(|e| BackendError::Malformed(e.to_string()))
}
