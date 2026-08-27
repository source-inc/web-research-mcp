use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{wrap_untrusted, Source, UntrustedEnvelope};
use crate::mcp::{domain::WebScrapeArgs, error_envelope, WebResearchMcp};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, FetchRecord, Store};

#[tool_router(router = scrape_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Fetch a single URL as LLM-ready markdown via Firecrawl. mode=\"static\" (default) for plain fetch, mode=\"rendered\" if JS is required. Returns untrusted-content envelope."
    )]
    pub async fn web_scrape_url(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebScrapeArgs>,
    ) -> Result<String, String> {
        self.web_scrape_url_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn web_scrape_url_inner(&self, args: WebScrapeArgs) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        if let Decision::Deny(r) = self.policy.check_family(Family::Extract) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_scrape_url", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "extract family disabled",
                Some(audit_id),
            ));
        }
        if let Decision::Deny(r) = self.policy.check_domain(&args.url) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_scrape_url", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "domain policy",
                Some(audit_id),
            ));
        }
        let _g = match self.policy.try_acquire_inflight() {
            Some(g) => g,
            None => {
                self.metrics
                    .policy_denials
                    .with_label_values(&["inflight_full"])
                    .inc();
                self.audit_denial(&audit_id, "web_scrape_url", &args, "inflight_full", now)
                    .await;
                return Ok(error_envelope(
                    "policy_denied",
                    "inflight_full",
                    "concurrent fetch limit reached",
                    Some(audit_id),
                ));
            }
        };
        let rendered = matches!(args.mode.as_deref(), Some("rendered"));
        let timer = self
            .metrics
            .backend_latency
            .with_label_values(&["firecrawl", "scrape"])
            .start_timer();
        let res = self.firecrawl.scrape(&args.url, rendered).await;
        timer.observe_duration();
        match res {
            Ok(sr) => {
                let fetch_id = Store::new_fetch_id();
                let bytes = sr.markdown.len() as u64;
                if (bytes as usize) > self.policy.limits().scrape_max_bytes {
                    self.metrics
                        .policy_denials
                        .with_label_values(&["scrape_max_bytes"])
                        .inc();
                    self.audit_denial(&audit_id, "web_scrape_url", &args, "scrape_max_bytes", now)
                        .await;
                    return Ok(error_envelope(
                        "policy_denied",
                        "scrape_max_bytes",
                        "page exceeds byte cap",
                        Some(audit_id),
                    ));
                }
                let requested_url = args.url.clone();
                let final_url = sr
                    .metadata
                    .get("sourceURL")
                    .and_then(|value| value.as_str())
                    .unwrap_or(&sr.url)
                    .to_string();
                let wrapped = wrap_untrusted(&final_url, &sr.markdown);
                let content_hash = Store::content_hash(wrapped.as_bytes());
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_scrape_url", "ok"])
                    .inc();
                let _ = self
                    .store
                    .write_fetch(&FetchRecord {
                        fetch_id: fetch_id.clone(),
                        requested_url,
                        final_url: final_url.clone(),
                        mode: "scrape".into(),
                        requested_at: now,
                        completed_at: Utc::now(),
                        status: "ok".into(),
                        bytes,
                        content_hash: content_hash.clone(),
                        content_markdown: wrapped.clone(),
                        source_provider: "firecrawl-selfhosted".into(),
                        policy_decision: "allow".into(),
                        truncated: false,
                    })
                    .await;
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: Some(fetch_id.clone()),
                        tool: "web_scrape_url".into(),
                        args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                        outcome: "ok".into(),
                        error: None,
                        recorded_at: now,
                    })
                    .await;
                serde_json::to_string_pretty(&UntrustedEnvelope::new(
                    Source {
                        url: final_url,
                        fetched_at: now.to_rfc3339(),
                        tool: "web_scrape_url".into(),
                        provider: "firecrawl-selfhosted".into(),
                    },
                    serde_json::json!({
                        "fetch_id": fetch_id,
                        "content_hash": content_hash,
                        "bytes": bytes,
                        "truncated": false,
                        "content_markdown": wrapped,
                        "links": sr.links,
                        "metadata": sr.metadata,
                    }),
                    audit_id,
                ))
                .map_err(|e| e.to_string())
            }
            Err(e) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_scrape_url", "error"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_scrape_url".into(),
                        args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                        outcome: "error".into(),
                        error: Some(e.to_string()),
                        recorded_at: now,
                    })
                    .await;
                Ok(error_envelope(
                    "backend_error",
                    "firecrawl",
                    &e.to_string(),
                    Some(audit_id),
                ))
            }
        }
    }
}
