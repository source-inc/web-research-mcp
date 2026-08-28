use std::collections::HashSet;

use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};
use serde::Serialize;

use crate::envelope::{Source, UntrustedEnvelope};
use crate::store::{ResearchBundleRecord, Store};

use super::{
    domain::{SearchHit, WebCollectEvidenceArgs, WebScrapeArgs, WebSearchArgs, WebVerifyQuoteArgs},
    error_envelope, WebResearchMcp,
};

const MAX_QUERIES: usize = 6;
const DEFAULT_SOURCES: usize = 6;
const MAX_SOURCES: usize = 8;
const MAX_SCRAPE_ATTEMPTS: usize = 12;
const SEARCH_RESULTS_PER_QUERY: usize = 8;
const EXCERPT_CHARS: usize = 4_000;

#[derive(Debug, Serialize)]
struct BundleSource {
    url: String,
    title: String,
    snippet: String,
    published_at: Option<String>,
    fetched_at: String,
    fetch_id: String,
    content_hash: String,
    bytes: u64,
    excerpt: String,
    verified_quote: String,
    quote_verified: bool,
}

#[tool_router(router = bundle_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Execute one idempotent, hard-bounded evidence collection for a research assignment. Runs 1-6 real SearXNG searches, deduplicates candidates, and attempts at most 12 real Firecrawl scrapes to return up to 8 persisted sources with fetch_id, content_hash, compact excerpts, and a gateway-verified exact excerpt. Repeating the same assignment_id and inputs returns the stored bundle without new network calls. Prefer this over open-ended web_search/web_scrape_url loops."
    )]
    pub async fn web_collect_evidence(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebCollectEvidenceArgs>,
    ) -> Result<String, String> {
        self.web_collect_evidence_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn web_collect_evidence_inner(
        &self,
        mut args: WebCollectEvidenceArgs,
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        args.assignment_id = args.assignment_id.trim().to_string();
        let mut seen_queries = HashSet::new();
        args.queries = args
            .queries
            .into_iter()
            .map(|query| query.trim().to_string())
            .filter(|query| !query.is_empty())
            .filter(|query| seen_queries.insert(query.clone()))
            .collect();
        if args.assignment_id.is_empty() || args.assignment_id.len() > 256 {
            return Ok(error_envelope(
                "invalid_argument",
                "bad_assignment_id",
                "assignment_id must contain 1 through 256 characters",
                Some(audit_id),
            ));
        }
        if args.queries.is_empty() || args.queries.len() > MAX_QUERIES {
            return Ok(error_envelope(
                "invalid_argument",
                "bad_query_count",
                "queries must contain 1 through 6 non-empty entries",
                Some(audit_id),
            ));
        }
        let max_sources = args.max_sources.unwrap_or(DEFAULT_SOURCES);
        if !(1..=MAX_SOURCES).contains(&max_sources) {
            return Ok(error_envelope(
                "invalid_argument",
                "bad_source_count",
                "max_sources must be between 1 and 8",
                Some(audit_id),
            ));
        }

        let request_json = serde_json::to_vec(&args).map_err(|error| error.to_string())?;
        let request_hash = Store::content_hash(&request_json);
        let assignment_lock = {
            let mut locks = self.bundle_locks.lock().await;
            locks
                .entry(args.assignment_id.clone())
                .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _assignment_guard = assignment_lock.lock().await;

        if let Some(record) = self
            .store
            .read_research_bundle(&args.assignment_id)
            .await
            .map_err(|error| error.to_string())?
        {
            if record.request_hash != request_hash {
                return Ok(error_envelope(
                    "invalid_argument",
                    "assignment_conflict",
                    "assignment_id already has a bundle with different inputs",
                    Some(audit_id),
                ));
            }
            let mut cached: serde_json::Value =
                serde_json::from_str(&record.response_json).map_err(|error| error.to_string())?;
            cached["content"]["cached"] = serde_json::Value::Bool(true);
            self.metrics
                .tool_calls
                .with_label_values(&["web_collect_evidence", "cached"])
                .inc();
            return serde_json::to_string_pretty(&cached).map_err(|error| error.to_string());
        }

        let mut searches = Vec::with_capacity(args.queries.len());
        let mut failures = Vec::new();
        for query in &args.queries {
            let response = self
                .web_search_inner(WebSearchArgs {
                    query: query.clone(),
                    max_results: Some(SEARCH_RESULTS_PER_QUERY),
                    categories: None,
                    language: Some("en".into()),
                    time_range: args.time_range.clone(),
                    include_domains: args.include_domains.clone(),
                    exclude_domains: args.exclude_domains.clone(),
                })
                .await?;
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|error| error.to_string())?;
            if let Some(error) = value.get("error") {
                failures.push(serde_json::json!({
                    "operation": "search",
                    "query": query,
                    "error": error,
                }));
                searches.push(Vec::new());
                continue;
            }
            let hits: Vec<SearchHit> =
                serde_json::from_value(value.get("content").cloned().unwrap_or_default())
                    .map_err(|error| error.to_string())?;
            searches.push(hits);
        }

        let mut candidates = Vec::new();
        let mut seen_urls = HashSet::new();
        for rank in 0..SEARCH_RESULTS_PER_QUERY {
            for hits in &searches {
                let Some(hit) = hits.get(rank) else {
                    continue;
                };
                if seen_urls.insert(hit.url.clone()) {
                    candidates.push(hit.clone());
                }
            }
        }

        let mut sources = Vec::with_capacity(max_sources);
        let mut scrape_attempts = 0usize;
        for candidate in candidates {
            if sources.len() >= max_sources || scrape_attempts >= MAX_SCRAPE_ATTEMPTS {
                break;
            }
            scrape_attempts += 1;
            let response = self
                .web_scrape_url_inner(WebScrapeArgs {
                    url: candidate.url.clone(),
                    mode: Some("static".into()),
                })
                .await?;
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|error| error.to_string())?;
            if let Some(error) = value.get("error") {
                failures.push(serde_json::json!({
                    "operation": "scrape",
                    "url": candidate.url,
                    "error": error,
                }));
                continue;
            }
            let content = value.get("content").cloned().unwrap_or_default();
            let source = value.get("source").cloned().unwrap_or_default();
            let provenance = value.get("provenance").cloned().unwrap_or_default();
            let markdown = content
                .get("content_markdown")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let excerpt = markdown.chars().take(EXCERPT_CHARS).collect::<String>();
            let Some(fetch_id) = provenance
                .get("fetch_id")
                .and_then(serde_json::Value::as_str)
            else {
                failures.push(serde_json::json!({
                    "operation": "scrape",
                    "url": candidate.url,
                    "error": "missing fetch_id",
                }));
                continue;
            };
            let Some(content_hash) = provenance
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
            else {
                failures.push(serde_json::json!({
                    "operation": "scrape",
                    "url": candidate.url,
                    "error": "missing content_hash",
                }));
                continue;
            };
            let verified_quote = verification_quote(markdown);
            let quote_verified = if verified_quote.is_empty() {
                false
            } else {
                let verification = self
                    .web_verify_quote_inner(WebVerifyQuoteArgs {
                        fetch_id: fetch_id.to_string(),
                        quote: verified_quote.clone(),
                        expected_content_hash: Some(content_hash.to_string()),
                    })
                    .await?;
                serde_json::from_str::<serde_json::Value>(&verification)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/content/derived/verified")
                            .and_then(serde_json::Value::as_bool)
                    })
                    .unwrap_or(false)
            };
            sources.push(BundleSource {
                url: source
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&candidate.url)
                    .to_string(),
                title: candidate.title,
                snippet: candidate.snippet,
                published_at: candidate.published_at,
                fetched_at: source
                    .get("fetched_at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                fetch_id: fetch_id.to_string(),
                content_hash: content_hash.to_string(),
                bytes: content
                    .get("bytes")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
                excerpt,
                verified_quote,
                quote_verified,
            });
        }

        let now = Utc::now();
        let response = serde_json::to_string_pretty(&UntrustedEnvelope::new(
            Source {
                url: format!("research-assignment:{}", args.assignment_id),
                fetched_at: now.to_rfc3339(),
                tool: "web_collect_evidence".into(),
                provider: "searxng+firecrawl-selfhosted".into(),
            },
            serde_json::json!({
                "assignment_id": args.assignment_id,
                "cached": false,
                "search_count": args.queries.len(),
                "scrape_attempt_count": scrape_attempts,
                "source_count": sources.len(),
                "queries": args.queries,
                "sources": sources,
                "failures": failures,
            }),
            audit_id,
        ))
        .map_err(|error| error.to_string())?;
        self.store
            .write_research_bundle(&ResearchBundleRecord {
                assignment_id: args.assignment_id,
                request_hash,
                response_json: response.clone(),
            })
            .await
            .map_err(|error| error.to_string())?;
        self.metrics
            .tool_calls
            .with_label_values(&["web_collect_evidence", "ok"])
            .inc();
        Ok(response)
    }
}

fn verification_quote(markdown: &str) -> String {
    let body = markdown
        .split_once('\n')
        .map(|(_, body)| body)
        .unwrap_or(markdown);
    body.trim_start()
        .chars()
        .take(320)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_quote_skips_the_untrusted_envelope_header_and_is_bounded() {
        let body = format!("<<<UNTRUSTED_WEB_CONTENT:nonce>>>\n  {}  ", "x".repeat(500));
        let quote = verification_quote(&body);
        assert_eq!(quote.len(), 320);
        assert!(quote.bytes().all(|byte| byte == b'x'));
        assert!(body.contains(&quote));
    }
}
