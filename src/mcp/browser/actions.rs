use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};
use serde::Serialize;

use crate::backends::camofox::CamofoxClient;
use crate::mcp::{
    domain::{
        BrowserClickArgs, BrowserScreenshotArgs, BrowserScrollArgs, BrowserSessionArgs,
        BrowserTypeArgs,
    },
    error_envelope, WebResearchMcp,
};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, Store};

#[tool_router(router = browser_actions_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Get an accessibility-tree snapshot of the current page in a browser session."
    )]
    pub async fn browser_snapshot(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserSessionArgs>,
    ) -> Result<String, String> {
        self.run_browser_session_call("browser_snapshot", &args.session_id, |c| {
            let id = args.session_id.clone();
            let c = c.clone();
            async move { c.snapshot(&id).await }
        })
        .await
    }

    #[tool(
        description = "Capture a screenshot of the current page; full_page=true captures the entire scrollable area."
    )]
    pub async fn browser_screenshot(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserScreenshotArgs>,
    ) -> Result<String, String> {
        let full_page = args.full_page.unwrap_or(false);
        self.run_browser_session_call("browser_screenshot", &args.session_id, |c| {
            let id = args.session_id.clone();
            let c = c.clone();
            async move { c.screenshot(&id, full_page).await }
        })
        .await
    }

    #[tool(description = "Click on a target (CSS selector or snapshot ref) in a browser session.")]
    pub async fn browser_click(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserClickArgs>,
    ) -> Result<String, String> {
        self.run_browser_session_call("browser_click", &args.session_id, |c| {
            let id = args.session_id.clone();
            let target = args.target.clone();
            let c = c.clone();
            async move { c.click(&id, &target).await }
        })
        .await
    }

    #[tool(
        description = "Type text into a target (CSS selector or snapshot ref) in a browser session."
    )]
    pub async fn browser_type(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserTypeArgs>,
    ) -> Result<String, String> {
        self.run_browser_session_call("browser_type", &args.session_id, |c| {
            let id = args.session_id.clone();
            let target = args.target.clone();
            let text = args.text.clone();
            let c = c.clone();
            async move { c.type_text(&id, &target, &text).await }
        })
        .await
    }

    #[tool(
        description = "Scroll the current page in a browser session. direction is one of up|down|left|right."
    )]
    pub async fn browser_scroll(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserScrollArgs>,
    ) -> Result<String, String> {
        self.run_browser_session_call("browser_scroll", &args.session_id, |c| {
            let id = args.session_id.clone();
            let direction = args.direction.clone();
            let amount = args.amount;
            let c = c.clone();
            async move { c.scroll(&id, &direction, amount).await }
        })
        .await
    }
}

impl WebResearchMcp {
    async fn run_browser_session_call<F, Fut, T>(
        &self,
        tool: &str,
        session_id: &str,
        op: F,
    ) -> Result<String, String>
    where
        F: FnOnce(&CamofoxClient) -> Fut,
        Fut: std::future::Future<Output = Result<T, crate::backends::BackendError>>,
        T: Serialize,
    {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        if let Decision::Deny(r) = self.policy.check_family(Family::Browser) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            let _ = self
                .store
                .write_audit(&AuditRecord {
                    entry_id: audit_id.clone(),
                    fetch_id: None,
                    tool: tool.into(),
                    args_redacted: serde_json::json!({"session_id": session_id}),
                    outcome: "policy_denied".into(),
                    error: Some(r.clone()),
                    recorded_at: now,
                })
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "browser family disabled",
                Some(audit_id),
            ));
        }
        if let Err(e) = self.sessions.touch(session_id) {
            let kind = match e {
                crate::sessions::LookupError::NotFound => "not_found",
                crate::sessions::LookupError::Expired => "session_expired",
            };
            self.metrics
                .tool_calls
                .with_label_values(&[tool, kind])
                .inc();
            let _ = self
                .store
                .write_audit(&AuditRecord {
                    entry_id: audit_id.clone(),
                    fetch_id: None,
                    tool: tool.into(),
                    args_redacted: serde_json::json!({"session_id": session_id}),
                    outcome: kind.into(),
                    error: None,
                    recorded_at: now,
                })
                .await;
            return Ok(error_envelope(
                kind,
                kind,
                "session not found or expired",
                Some(audit_id),
            ));
        }
        let res = op(&self.camofox).await;
        match res {
            Ok(payload) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&[tool, "ok"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: tool.into(),
                        args_redacted: serde_json::json!({"session_id": session_id}),
                        outcome: "ok".into(),
                        error: None,
                        recorded_at: now,
                    })
                    .await;
                Ok(serde_json::to_string(&serde_json::json!({
                    "trust": "untrusted_web_evidence",
                    "source": {
                        "url": "",
                        "fetched_at": now.to_rfc3339(),
                        "tool": tool,
                        "provider": "camofox"
                    },
                    "content": payload,
                    "audit_id": audit_id,
                }))
                .unwrap_or_default())
            }
            Err(e) => {
                // Tear down the backend tab too, not just the registry slot, so a
                // failed action doesn't leak an open browser tab in Camoufox.
                let _ = self.camofox.close(session_id).await;
                self.sessions.remove(session_id);
                self.metrics
                    .browser_sessions_active
                    .set(self.sessions.count() as i64);
                self.metrics
                    .tool_calls
                    .with_label_values(&[tool, "error"])
                    .inc();
                let _ = self
                    .store
                    .write_audit(&AuditRecord {
                        entry_id: audit_id.clone(),
                        fetch_id: None,
                        tool: tool.into(),
                        args_redacted: serde_json::json!({"session_id": session_id}),
                        outcome: "session_force_closed".into(),
                        error: Some(e.to_string()),
                        recorded_at: now,
                    })
                    .await;
                Ok(error_envelope(
                    "backend_error",
                    "camofox",
                    &e.to_string(),
                    Some(audit_id),
                ))
            }
        }
    }
}
