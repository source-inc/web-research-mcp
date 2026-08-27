use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{wrap_untrusted, Source, UntrustedEnvelope};
use crate::mcp::{domain::WebCrawlArgs, error_envelope, WebResearchMcp};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, Store};

#[tool_router(router = crawl_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Crawl a site rooted at URL with explicit depth + page caps via Firecrawl. Both caps are required and enforced by policy."
    )]
    pub async fn web_crawl_site(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebCrawlArgs>,
    ) -> Result<String, String> {
        self.web_crawl_site_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn web_crawl_site_inner(&self, args: WebCrawlArgs) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        if let Decision::Deny(r) = self.policy.check_family(Family::Crawl) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_crawl_site", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "crawl family disabled",
                Some(audit_id),
            ));
        }
        if let Decision::Deny(r) = self.policy.check_caps_crawl(args.max_depth, args.max_pages) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_crawl_site", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "crawl caps",
                Some(audit_id),
            ));
        }
        if let Decision::Deny(r) = self.policy.check_domain(&args.url) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_crawl_site", &args, &r, now)
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
                self.audit_denial(&audit_id, "web_crawl_site", &args, "inflight_full", now)
                    .await;
                return Ok(error_envelope(
                    "policy_denied",
                    "inflight_full",
                    "concurrent fetch limit reached",
                    Some(audit_id),
                ));
            }
        };
        let timer = self
            .metrics
            .backend_latency
            .with_label_values(&["firecrawl", "crawl"])
            .start_timer();
        let res = self
            .firecrawl
            .crawl(&args.url, args.max_depth, args.max_pages)
            .await;
        timer.observe_duration();
        match res {
            Ok(pages) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_crawl_site", "ok"])
                    .inc();
                let wrapped: Vec<_> = pages
                    .into_iter()
                    .map(|p| {
                        serde_json::json!({
                            "url": p.url,
                            "content_markdown": wrap_untrusted(&p.url, &p.markdown),
                            "links": p.links,
                            "metadata": p.metadata,
                        })
                    })
                    .collect();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_crawl_site".into(),
                        args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                        outcome: "ok".into(),
                        error: None,
                        recorded_at: now,
                    })
                    .await;
                serde_json::to_string_pretty(&UntrustedEnvelope::new(
                    Source {
                        url: args.url,
                        fetched_at: now.to_rfc3339(),
                        tool: "web_crawl_site".into(),
                        provider: "firecrawl-selfhosted".into(),
                    },
                    wrapped,
                    audit_id,
                ))
                .map_err(|e| e.to_string())
            }
            Err(e) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_crawl_site", "error"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_crawl_site".into(),
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
