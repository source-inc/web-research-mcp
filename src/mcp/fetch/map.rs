use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{Source, UntrustedEnvelope};
use crate::mcp::{domain::WebMapArgs, error_envelope, WebResearchMcp};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, Store};

#[tool_router(router = map_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Discover URLs under a site via Firecrawl map. Returns a list of URLs; no page content. Use web_scrape_url to fetch individual pages."
    )]
    pub async fn web_map_site(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebMapArgs>,
    ) -> Result<String, String> {
        self.web_map_site_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn web_map_site_inner(&self, args: WebMapArgs) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        if let Decision::Deny(r) = self.policy.check_family(Family::Extract) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "web_map_site", &args, &r, now)
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
            self.audit_denial(&audit_id, "web_map_site", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "domain policy",
                Some(audit_id),
            ));
        }
        let max = args
            .max_pages
            .unwrap_or(self.policy.limits().crawl_max_pages);
        let _g = match self.policy.try_acquire_inflight() {
            Some(g) => g,
            None => {
                self.metrics
                    .policy_denials
                    .with_label_values(&["inflight_full"])
                    .inc();
                self.audit_denial(&audit_id, "web_map_site", &args, "inflight_full", now)
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
            .with_label_values(&["firecrawl", "map"])
            .start_timer();
        let res = self.firecrawl.map(&args.url, max).await;
        timer.observe_duration();
        match res {
            Ok(entries) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_map_site", "ok"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_map_site".into(),
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
                        tool: "web_map_site".into(),
                        provider: "firecrawl-selfhosted".into(),
                    },
                    entries,
                    audit_id,
                ))
                .map_err(|e| e.to_string())
            }
            Err(e) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_map_site", "error"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_map_site".into(),
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
