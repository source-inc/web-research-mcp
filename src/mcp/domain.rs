use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebSearchArgs {
    /// Search query.
    pub query: String,
    /// Max results (capped by policy).
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Optional SearXNG categories filter.
    #[serde(default)]
    pub categories: Option<String>,
    /// Optional SearXNG language code (for example "en" or "en-US").
    #[serde(default)]
    pub language: Option<String>,
    /// Optional freshness window: "day", "month", or "year".
    #[serde(default)]
    pub time_range: Option<String>,
    /// Restrict candidates to these domains. Multiple domains are ORed.
    #[serde(default)]
    pub include_domains: Vec<String>,
    /// Exclude candidates from these domains.
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub rank: usize,
    pub url: String,
    pub title: String,
    pub snippet: String,
    pub engine: String,
    pub score: Option<f64>,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebScrapeArgs {
    /// URL to scrape.
    pub url: String,
    /// "static" (default) or "rendered" (uses Firecrawl wait).
    #[serde(default)]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebGetFetchArgs {
    /// Stable fetch identifier returned by web_scrape_url.
    pub fetch_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebFindInFetchArgs {
    /// Stable fetch identifier returned by web_scrape_url.
    pub fetch_id: String,
    /// Exact text to find in the stored evidence.
    pub needle: String,
    /// Maximum number of matches to return (default 10, maximum 50).
    #[serde(default)]
    pub max_matches: Option<usize>,
    /// Context characters on either side of each match (default 160, maximum 1000).
    #[serde(default)]
    pub context_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebVerifyQuoteArgs {
    /// Stable fetch identifier returned by web_scrape_url.
    pub fetch_id: String,
    /// Exact quote to verify against the stored evidence.
    pub quote: String,
    /// Optional expected sha256 content hash to protect against stale evidence.
    #[serde(default)]
    pub expected_content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebMapArgs {
    pub url: String,
    #[serde(default)]
    pub max_pages: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebCrawlArgs {
    pub url: String,
    pub max_depth: usize,
    pub max_pages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserOpenArgs {
    #[serde(default)]
    pub start_url: Option<String>,
    #[serde(default)]
    pub session_ttl: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserSessionArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserScreenshotArgs {
    pub session_id: String,
    #[serde(default)]
    pub full_page: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserClickArgs {
    pub session_id: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserTypeArgs {
    pub session_id: String,
    pub target: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserScrollArgs {
    pub session_id: String,
    pub direction: String,
    #[serde(default)]
    pub amount: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BrowserCookieImportArgs {
    pub session_id: String,
    pub cookies: serde_json::Value,
}
