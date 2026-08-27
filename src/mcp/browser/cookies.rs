use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::mcp::{domain::BrowserCookieImportArgs, error_envelope, WebResearchMcp};
use crate::store::{AuditRecord, Store};

#[tool_router(router = browser_cookies_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Import cookies into a browser session. DISABLED in v1; always returns policy_denied."
    )]
    pub async fn browser_import_cookies(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserCookieImportArgs>,
    ) -> Result<String, String> {
        self.browser_import_cookies_inner(args).await
    }
}

impl WebResearchMcp {
    pub async fn browser_import_cookies_inner(
        &self,
        args: BrowserCookieImportArgs,
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        let reason = if self.cookie_import_enabled {
            "not_implemented"
        } else {
            "disabled_by_policy"
        };
        self.metrics
            .policy_denials
            .with_label_values(&[reason])
            .inc();
        let _ = self
            .store
            .write_audit(&AuditRecord {
                entry_id: audit_id.clone(),
                fetch_id: None,
                tool: "browser_import_cookies".into(),
                args_redacted: serde_json::json!({
                    "session_id": args.session_id,
                    "cookie_count": args.cookies.as_array().map(|a| a.len()).unwrap_or(0)
                }),
                outcome: "policy_denied".into(),
                error: Some(reason.into()),
                recorded_at: now,
            })
            .await;
        Ok(error_envelope(
            "policy_denied",
            reason,
            "cookie import is disabled in v1",
            Some(audit_id),
        ))
    }
}
