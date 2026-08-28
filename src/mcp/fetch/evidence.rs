use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{Provenance, Source, UntrustedEnvelope};
use crate::mcp::{
    domain::{WebFindInFetchArgs, WebGetFetchArgs, WebVerifyQuoteArgs},
    error_envelope, WebResearchMcp,
};
use crate::store::{AuditRecord, FetchRecord, Store};

#[tool_router(router = evidence_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Load previously fetched evidence by fetch_id. Returns the exact stored content and its content_hash without contacting the web."
    )]
    pub async fn web_get_fetch(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebGetFetchArgs>,
    ) -> Result<String, String> {
        self.web_get_fetch_inner(args).await
    }

    #[tool(
        description = "Find exact text inside stored evidence and return bounded excerpts with byte offsets. Use this before selecting quotations."
    )]
    pub async fn web_find_in_fetch(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebFindInFetchArgs>,
    ) -> Result<String, String> {
        self.web_find_in_fetch_inner(args).await
    }

    #[tool(
        description = "Verify that an exact quote occurs in stored evidence, optionally requiring a matching content_hash. Use immediately before citing a quote."
    )]
    pub async fn web_verify_quote(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<WebVerifyQuoteArgs>,
    ) -> Result<String, String> {
        self.web_verify_quote_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn web_get_fetch_inner(&self, args: WebGetFetchArgs) -> Result<String, String> {
        let Some(record) = self.load_fetch(&args.fetch_id).await? else {
            return Ok(error_envelope(
                "not_found",
                "unknown_fetch_id",
                "no stored evidence exists for this fetch_id",
                None,
            ));
        };
        self.evidence_response("web_get_fetch", record, serde_json::Value::Null, true)
            .await
    }

    pub async fn web_find_in_fetch_inner(
        &self,
        args: WebFindInFetchArgs,
    ) -> Result<String, String> {
        if args.needle.is_empty() {
            return Ok(error_envelope(
                "invalid_argument",
                "empty_needle",
                "needle must not be empty",
                None,
            ));
        }
        let Some(record) = self.load_fetch(&args.fetch_id).await? else {
            return Ok(error_envelope(
                "not_found",
                "unknown_fetch_id",
                "no stored evidence exists for this fetch_id",
                None,
            ));
        };
        let max_matches = args.max_matches.unwrap_or(10).clamp(1, 50);
        let context_chars = args.context_chars.unwrap_or(160).clamp(0, 1000);
        let matches = excerpts(
            &record.content_markdown,
            &args.needle,
            max_matches,
            context_chars,
        );
        self.evidence_response(
            "web_find_in_fetch",
            record,
            serde_json::json!({"needle": args.needle, "matches": matches}),
            false,
        )
        .await
    }

    pub async fn web_verify_quote_inner(&self, args: WebVerifyQuoteArgs) -> Result<String, String> {
        if args.quote.is_empty() {
            return Ok(error_envelope(
                "invalid_argument",
                "empty_quote",
                "quote must not be empty",
                None,
            ));
        }
        let Some(record) = self.load_fetch(&args.fetch_id).await? else {
            return Ok(error_envelope(
                "not_found",
                "unknown_fetch_id",
                "no stored evidence exists for this fetch_id",
                None,
            ));
        };
        let hash_matches = args
            .expected_content_hash
            .as_ref()
            .map(|expected| expected == &record.content_hash)
            .unwrap_or(true);
        let positions: Vec<usize> = record
            .content_markdown
            .match_indices(&args.quote)
            .map(|(offset, _)| offset)
            .take(51)
            .collect();
        let verified = hash_matches && !positions.is_empty();
        self.evidence_response(
            "web_verify_quote",
            record,
            serde_json::json!({
                "verified": verified,
                "hash_matches": hash_matches,
                "match_count": positions.len().min(50),
                "match_offsets": positions.into_iter().take(50).collect::<Vec<_>>(),
            }),
            false,
        )
        .await
    }

    async fn load_fetch(&self, fetch_id: &str) -> Result<Option<FetchRecord>, String> {
        self.store.read_fetch(fetch_id).await.map_err(|error| {
            serde_json::to_string(&serde_json::json!({
                "error": {"kind": "store_error", "detail": error.to_string()}
            }))
            .unwrap_or_else(|_| "store error".into())
        })
    }

    async fn evidence_response(
        &self,
        tool: &str,
        record: FetchRecord,
        derived: serde_json::Value,
        include_stored_content: bool,
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        self.metrics
            .tool_calls
            .with_label_values(&[tool, "ok"])
            .inc();
        let _ = self
            .store
            .write_audit(&AuditRecord {
                entry_id: audit_id.clone(),
                fetch_id: Some(record.fetch_id.clone()),
                tool: tool.into(),
                args_redacted: serde_json::json!({"fetch_id": record.fetch_id}),
                outcome: "ok".into(),
                error: None,
                recorded_at: now,
            })
            .await;
        serde_json::to_string_pretty(&UntrustedEnvelope::new_with_provenance(
            Source {
                url: record.final_url.clone(),
                fetched_at: record.completed_at.to_rfc3339(),
                tool: tool.into(),
                provider: record.source_provider.clone(),
            },
            Provenance {
                fetch_id: record.fetch_id.clone(),
                content_hash: record.content_hash.clone(),
            },
            evidence_content(&record, derived, include_stored_content),
            audit_id,
        ))
        .map_err(|error| error.to_string())
    }
}

fn evidence_content(
    record: &FetchRecord,
    derived: serde_json::Value,
    include_stored_content: bool,
) -> serde_json::Value {
    let mut content = serde_json::json!({
        "fetch_id": record.fetch_id,
        "content_hash": record.content_hash,
        "requested_url": record.requested_url,
        "final_url": record.final_url,
        "bytes": record.bytes,
        "truncated": record.truncated,
        "derived": derived,
    });
    if include_stored_content {
        content["content_markdown"] = serde_json::Value::String(record.content_markdown.clone());
    }
    content
}

fn excerpts(body: &str, needle: &str, limit: usize, context: usize) -> Vec<serde_json::Value> {
    body.match_indices(needle)
        .take(limit)
        .map(|(offset, _)| {
            let start = floor_char_boundary(body, offset.saturating_sub(context));
            let end = ceil_char_boundary(body, (offset + needle.len() + context).min(body.len()));
            serde_json::json!({
                "byte_offset": offset,
                "excerpt": &body[start..end],
            })
        })
        .collect()
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_with_body(body: String) -> FetchRecord {
        let now = Utc::now();
        FetchRecord {
            fetch_id: "fetch-123".into(),
            requested_url: "https://example.com/source".into(),
            final_url: "https://example.com/source".into(),
            mode: "scrape".into(),
            requested_at: now,
            completed_at: now,
            status: "ok".into(),
            bytes: body.len() as u64,
            content_hash: Store::content_hash(body.as_bytes()),
            content_markdown: body,
            source_provider: "fixture".into(),
            policy_decision: "allow".into(),
            truncated: false,
        }
    }

    #[test]
    fn excerpts_are_utf8_safe() {
        let body = "zero café needle τέλος";
        let found = excerpts(body, "needle", 1, 3);
        assert_eq!(found.len(), 1);
        assert!(found[0]["excerpt"].as_str().unwrap().contains("needle"));
    }

    #[test]
    fn derived_evidence_response_does_not_repeat_the_stored_page() {
        let record = record_with_body(format!(
            "{}needle{}",
            "a".repeat(50_000),
            "b".repeat(50_000)
        ));
        let matches = excerpts(&record.content_markdown, "needle", 1, 160);
        let content = evidence_content(
            &record,
            serde_json::json!({"needle": "needle", "matches": matches}),
            false,
        );
        let serialized = serde_json::to_vec(&content).expect("serialize compact evidence response");

        assert!(content.get("content_markdown").is_none());
        assert_eq!(content["fetch_id"], "fetch-123");
        assert_eq!(content["derived"]["matches"].as_array().unwrap().len(), 1);
        assert!(
            serialized.len() < 2_000,
            "derived response was {} bytes",
            serialized.len()
        );
    }

    #[test]
    fn get_fetch_response_keeps_the_stored_page() {
        let record = record_with_body("stored body".into());
        let content = evidence_content(&record, serde_json::Value::Null, true);

        assert_eq!(content["content_markdown"], "stored body");
    }
}
