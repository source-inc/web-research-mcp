use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{Source, UntrustedEnvelope};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, Store};

use super::{
    domain::{SearchHit, WebSearchArgs},
    error_envelope, WebResearchMcp,
};

#[tool_router(router = search_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Search the public web via SearXNG. Returns candidate sources only (URL/title/snippet); no page content. Use web_scrape_url to fetch a page."
    )]
    pub async fn web_search(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebSearchArgs>,
    ) -> Result<String, String> {
        self.web_search_inner(args).await
    }
}

/// Context-free inner implementation. Tests call this directly to avoid needing
/// to construct an `rmcp::service::RequestContext`.
impl WebResearchMcp {
    pub async fn web_search_inner(&self, args: WebSearchArgs) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();

        if let Decision::Deny(reason) = self.policy.check_family(Family::Search) {
            self.metrics
                .policy_denials
                .with_label_values(&[&reason])
                .inc();
            self.audit_denial(&audit_id, "web_search", &args, &reason, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &reason,
                "search family disabled",
                Some(audit_id),
            ));
        }
        let max = args
            .max_results
            .unwrap_or(self.policy.limits().search_max_results);
        if let Decision::Deny(reason) = self.policy.check_caps_search(max) {
            self.metrics
                .policy_denials
                .with_label_values(&[&reason])
                .inc();
            self.audit_denial(&audit_id, "web_search", &args, &reason, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &reason,
                "result cap exceeded",
                Some(audit_id),
            ));
        }
        if let Some(time_range) = args.time_range.as_deref() {
            if !matches!(time_range, "day" | "month" | "year") {
                return Ok(error_envelope(
                    "invalid_argument",
                    "bad_time_range",
                    "time_range must be day, month, or year",
                    Some(audit_id),
                ));
            }
        }
        let search_query = match search_query(&args) {
            Ok(query) => query,
            Err(detail) => {
                return Ok(error_envelope(
                    "invalid_argument",
                    "bad_domain_filter",
                    &detail,
                    Some(audit_id),
                ))
            }
        };
        let _guard = match self.policy.try_acquire_inflight() {
            Some(g) => g,
            None => {
                let reason = "inflight_full".to_string();
                self.metrics
                    .policy_denials
                    .with_label_values(&[&reason])
                    .inc();
                self.audit_denial(&audit_id, "web_search", &args, &reason, now)
                    .await;
                return Ok(error_envelope(
                    "policy_denied",
                    &reason,
                    "concurrent fetch limit reached",
                    Some(audit_id),
                ));
            }
        };
        let timer = self
            .metrics
            .backend_latency
            .with_label_values(&["searxng", "search"])
            .start_timer();
        let res = self
            .searxng
            .search(
                &search_query,
                max,
                args.categories.as_deref(),
                args.language.as_deref(),
                args.time_range.as_deref(),
            )
            .await;
        timer.observe_duration();
        match res {
            Ok(hits) => {
                let mapped: Vec<SearchHit> = hits
                    .into_iter()
                    .enumerate()
                    .map(|(index, h)| SearchHit {
                        rank: index + 1,
                        url: h.url,
                        title: h.title,
                        snippet: h.snippet,
                        engine: h.engine,
                        score: h.score,
                        published_at: h.published_at,
                    })
                    .collect();
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_search", "ok"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_search".into(),
                        args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                        outcome: "ok".into(),
                        error: None,
                        recorded_at: now,
                    })
                    .await;
                serde_json::to_string_pretty(&UntrustedEnvelope::new(
                    Source {
                        url: format!("search:{}", args.query),
                        fetched_at: now.to_rfc3339(),
                        tool: "web_search".into(),
                        provider: "searxng".into(),
                    },
                    mapped,
                    audit_id,
                ))
                .map_err(|e| e.to_string())
            }
            Err(e) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["web_search", "error"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: "web_search".into(),
                        args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                        outcome: "error".into(),
                        error: Some(e.to_string()),
                        recorded_at: now,
                    })
                    .await;
                Ok(error_envelope(
                    "backend_error",
                    "searxng",
                    &e.to_string(),
                    Some(audit_id),
                ))
            }
        }
    }
}

fn search_query(args: &WebSearchArgs) -> Result<String, String> {
    let includes = args
        .include_domains
        .iter()
        .map(|domain| checked_domain(domain).map(|domain| format!("site:{domain}")))
        .collect::<Result<Vec<_>, _>>()?;
    let excludes = args
        .exclude_domains
        .iter()
        .map(|domain| checked_domain(domain).map(|domain| format!("-site:{domain}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut query = args.query.clone();
    if !includes.is_empty() {
        query.push_str(" (");
        query.push_str(&includes.join(" OR "));
        query.push(')');
    }
    for exclude in excludes {
        query.push(' ');
        query.push_str(&exclude);
    }
    Ok(query)
}

fn checked_domain(domain: &str) -> Result<&str, String> {
    let domain = domain.trim().trim_start_matches("*.");
    if domain.is_empty()
        || domain.len() > 253
        || domain.starts_with('.')
        || domain.ends_with('.')
        || !domain
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
    {
        return Err(format!("invalid domain filter: {domain}"));
    }
    Ok(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_query_composes_domain_filters() {
        let args = WebSearchArgs {
            query: "agent research".into(),
            max_results: None,
            categories: None,
            language: None,
            time_range: None,
            include_domains: vec!["example.com".into(), "docs.rs".into()],
            exclude_domains: vec!["spam.test".into()],
        };
        assert_eq!(
            search_query(&args).unwrap(),
            "agent research (site:example.com OR site:docs.rs) -site:spam.test"
        );
    }
}
