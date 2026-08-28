use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};
use serde::Serialize;

use crate::envelope::{Source, UntrustedEnvelope};
use crate::store::{ResearchBundleRecord, Store};

use super::relevance::{assess_content, categories_for_query, rank_candidates, CandidateRejection};
use super::{
    domain::{SearchHit, WebCollectEvidenceArgs, WebScrapeArgs, WebSearchArgs, WebVerifyQuoteArgs},
    error_envelope, WebResearchMcp,
};

const MAX_QUERIES: usize = 6;
const DEFAULT_SOURCES: usize = 6;
const MAX_SOURCES: usize = 8;
const MAX_SCRAPE_ATTEMPTS: usize = 12;
const SEARCH_RESULTS_PER_QUERY: usize = 20;
const MAX_REPORTED_REJECTIONS: usize = 40;

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
    matched_query: String,
    retrieval_queries: Vec<String>,
    search_engines: Vec<String>,
    candidate_relevance_score: f64,
    content_relevance_score: f64,
    extraction_method: &'static str,
    verified_quote: String,
    quote_verified: bool,
    content_integrity_verified: bool,
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

        let started_at = Instant::now();
        let mut searches = Vec::with_capacity(args.queries.len());
        let mut failures = Vec::new();
        let mut search_degradation = Vec::new();
        for query in &args.queries {
            let response = self
                .web_search_inner(WebSearchArgs {
                    query: query.clone(),
                    max_results: Some(SEARCH_RESULTS_PER_QUERY),
                    categories: categories_for_query(query).map(str::to_string),
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
            if let Some(unresponsive) = value.pointer("/diagnostics/unresponsive_engines") {
                if unresponsive
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
                {
                    search_degradation.push(serde_json::json!({
                        "query": query,
                        "unresponsive_engines": unresponsive,
                    }));
                }
            }
            searches.push(hits);
        }

        let ranking = rank_candidates(&args.queries, &searches);
        self.metrics
            .evidence_candidates
            .with_label_values(&["preview", "accepted"])
            .inc_by(ranking.candidates.len() as u64);
        self.metrics
            .evidence_candidates
            .with_label_values(&["preview", "rejected"])
            .inc_by(ranking.rejected.len() as u64);
        let candidate_count = ranking.candidates.len() + ranking.rejected.len();
        let mut rejections = ranking.rejected;

        let mut sources = Vec::with_capacity(max_sources);
        let mut scrape_attempts = 0usize;
        for candidate in ranking.candidates {
            if sources.len() >= max_sources || scrape_attempts >= MAX_SCRAPE_ATTEMPTS {
                break;
            }
            scrape_attempts += 1;
            let response = self
                .web_scrape_url_inner(WebScrapeArgs {
                    url: candidate.hit.url.clone(),
                    mode: Some("static".into()),
                })
                .await?;
            let value: serde_json::Value =
                serde_json::from_str(&response).map_err(|error| error.to_string())?;
            if let Some(error) = value.get("error") {
                failures.push(serde_json::json!({
                    "operation": "scrape",
                    "url": candidate.hit.url,
                    "error": error,
                }));
                self.metrics
                    .evidence_candidates
                    .with_label_values(&["fetch", "error"])
                    .inc();
                continue;
            }
            let content = value.get("content").cloned().unwrap_or_default();
            let source = value.get("source").cloned().unwrap_or_default();
            let provenance = value.get("provenance").cloned().unwrap_or_default();
            let markdown = content
                .get("content_markdown")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let final_url = source
                .get("url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&candidate.hit.url);
            let assessment = assess_content(
                markdown,
                final_url,
                &candidate.hit.title,
                &candidate.matched_queries,
            );
            if !assessment.accepted {
                let reason = assessment.reason.unwrap_or("content_validation_failed");
                tracing::info!(
                    assignment_id = %args.assignment_id,
                    url = %candidate.hit.url,
                    candidate_score = candidate.score,
                    content_score = assessment.relevance_score,
                    reason,
                    "rejected fetched evidence candidate"
                );
                rejections.push(CandidateRejection {
                    url: candidate.hit.url,
                    stage: "content",
                    reason: reason.to_string(),
                    score: assessment.relevance_score,
                });
                self.metrics
                    .evidence_candidates
                    .with_label_values(&["content", "rejected"])
                    .inc();
                continue;
            }
            let Some(fetch_id) = provenance
                .get("fetch_id")
                .and_then(serde_json::Value::as_str)
            else {
                failures.push(serde_json::json!({
                    "operation": "scrape",
                    "url": candidate.hit.url,
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
                    "url": candidate.hit.url,
                    "error": "missing content_hash",
                }));
                continue;
            };
            let verified_quote = assessment.verification_quote;
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
            if !quote_verified {
                rejections.push(CandidateRejection {
                    url: candidate.hit.url,
                    stage: "integrity",
                    reason: "quote_verification_failed".to_string(),
                    score: assessment.relevance_score,
                });
                self.metrics
                    .evidence_candidates
                    .with_label_values(&["integrity", "rejected"])
                    .inc();
                continue;
            }
            tracing::info!(
                assignment_id = %args.assignment_id,
                url = %final_url,
                candidate_score = candidate.score,
                content_score = assessment.relevance_score,
                engines = ?candidate.search_engines,
                "accepted evidence source"
            );
            sources.push(BundleSource {
                url: final_url.to_string(),
                title: candidate.hit.title,
                snippet: candidate.hit.snippet,
                published_at: candidate.hit.published_at,
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
                excerpt: assessment.excerpt,
                matched_query: assessment.matched_query,
                retrieval_queries: candidate.matched_queries,
                search_engines: candidate.search_engines,
                candidate_relevance_score: candidate.score,
                content_relevance_score: assessment.relevance_score,
                extraction_method: "firecrawl-selfhosted",
                verified_quote,
                quote_verified,
                content_integrity_verified: quote_verified,
            });
            self.metrics
                .evidence_candidates
                .with_label_values(&["content", "accepted"])
                .inc();
        }

        let accepted_engines = sources
            .iter()
            .flat_map(|source| source.search_engines.iter().cloned())
            .collect::<BTreeSet<_>>();
        let evidence_shortfall = sources.len() < max_sources;
        let outcome = if sources.is_empty() {
            "empty"
        } else if evidence_shortfall {
            "shortfall"
        } else {
            "complete"
        };
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
                "candidate_count": candidate_count,
                "scrape_attempt_count": scrape_attempts,
                "source_count": sources.len(),
                "requested_source_count": max_sources,
                "evidence_shortfall": evidence_shortfall,
                "accepted_search_engines": accepted_engines,
                "queries": args.queries,
                "sources": sources,
                "failures": failures,
                "search_degradation": search_degradation,
                "rejections": rejections.into_iter().take(MAX_REPORTED_REJECTIONS).collect::<Vec<_>>(),
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
        self.metrics
            .evidence_bundle_duration
            .with_label_values(&[outcome])
            .observe(started_at.elapsed().as_secs_f64());
        Ok(response)
    }
}
