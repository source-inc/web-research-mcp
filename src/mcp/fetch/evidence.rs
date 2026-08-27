use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::envelope::{Source, UntrustedEnvelope};
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
        self.evidence_response("web_get_fetch", record, serde_json::Value::Null)
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
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
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
        serde_json::to_string_pretty(&UntrustedEnvelope::new(
            Source {
                url: record.final_url.clone(),
                fetched_at: record.completed_at.to_rfc3339(),
                tool: tool.into(),
                provider: record.source_provider.clone(),
            },
            serde_json::json!({
                "fetch_id": record.fetch_id,
                "content_hash": record.content_hash,
                "requested_url": record.requested_url,
                "final_url": record.final_url,
                "bytes": record.bytes,
                "truncated": record.truncated,
                "content_markdown": record.content_markdown,
                "derived": derived,
            }),
            audit_id,
        ))
        .map_err(|error| error.to_string())
    }
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

    #[test]
    fn excerpts_are_utf8_safe() {
        let body = "zero café needle τέλος";
        let found = excerpts(body, "needle", 1, 3);
        assert_eq!(found.len(), 1);
        assert!(found[0]["excerpt"].as_str().unwrap().contains("needle"));
    }
}
