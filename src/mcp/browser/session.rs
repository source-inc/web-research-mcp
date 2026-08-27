use std::time::Duration;

use chrono::Utc;
use rmcp::{
    handler::server::wrapper::Parameters, service::RequestContext, tool, tool_router, RoleServer,
};

use crate::mcp::{
    domain::{BrowserOpenArgs, BrowserSessionArgs},
    error_envelope, WebResearchMcp,
};
use crate::policy::{Decision, Family};
use crate::store::{AuditRecord, Store};

#[tool_router(router = browser_session_router, vis = "pub(crate)")]
impl WebResearchMcp {
    #[tool(
        description = "Open a Camoufox browser session. Optional start_url is loaded immediately. Returns a session_id used for all subsequent browser_* calls. Sessions auto-close after session_ttl seconds or idle timeout."
    )]
    pub async fn browser_open(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserOpenArgs>,
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        if let Decision::Deny(r) = self.policy.check_family(Family::Browser) {
            self.metrics.policy_denials.with_label_values(&[&r]).inc();
            self.audit_denial(&audit_id, "browser_open", &args, &r, now)
                .await;
            return Ok(error_envelope(
                "policy_denied",
                &r,
                "browser family disabled",
                Some(audit_id),
            ));
        }
        if let Some(u) = &args.start_url {
            if let Decision::Deny(r) = self.policy.check_domain(u) {
                self.metrics.policy_denials.with_label_values(&[&r]).inc();
                self.audit_denial(&audit_id, "browser_open", &args, &r, now)
                    .await;
                return Ok(error_envelope(
                    "policy_denied",
                    &r,
                    "domain policy",
                    Some(audit_id),
                ));
            }
        }
        let timer = self
            .metrics
            .backend_latency
            .with_label_values(&["camofox", "open"])
            .start_timer();
        let opened = self.camofox.open(args.start_url.as_deref()).await;
        timer.observe_duration();
        let session_id = match opened {
            Ok(r) => r.session_id,
            Err(e) => {
                self.metrics
                    .tool_calls
                    .with_label_values(&["browser_open", "error"])
                    .inc();
                return Ok(error_envelope(
                    "backend_error",
                    "camofox",
                    &e.to_string(),
                    Some(audit_id),
                ));
            }
        };
        let ttl = Duration::from_secs(
            args.session_ttl
                .unwrap_or(self.policy.limits().browser_session_ttl_secs)
                .min(1800),
        );
        let idle = Duration::from_secs(self.policy.limits().browser_session_idle_secs);
        if self
            .sessions
            .allocate(session_id.clone(), ttl, idle)
            .is_err()
        {
            let _ = self.camofox.close(&session_id).await;
            self.metrics
                .policy_denials
                .with_label_values(&["browser_max_concurrent"])
                .inc();
            self.audit_denial(
                &audit_id,
                "browser_open",
                &args,
                "browser_max_concurrent",
                now,
            )
            .await;
            return Ok(error_envelope(
                "policy_denied",
                "browser_max_concurrent",
                "browser session cap reached",
                Some(audit_id),
            ));
        }
        self.metrics
            .browser_sessions_active
            .set(self.sessions.count() as i64);
        self.metrics
            .tool_calls
            .with_label_values(&["browser_open", "ok"])
            .inc();
        let _ = self
            .store
            .write_audit(&AuditRecord {
                entry_id: audit_id.clone(),
                fetch_id: None,
                tool: "browser_open".into(),
                args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                outcome: "session_open".into(),
                error: None,
                recorded_at: now,
            })
            .await;
        Ok(serde_json::to_string(&serde_json::json!({
            "trust": "untrusted_web_evidence",
            "source": {
                "url": args.start_url.clone().unwrap_or_default(),
                "fetched_at": now.to_rfc3339(),
                "tool": "browser_open",
                "provider": "camofox"
            },
            "content": { "session_id": session_id },
            "audit_id": audit_id,
        }))
        .unwrap_or_default())
    }

    #[tool(description = "Close a browser session and free its slot.")]
    pub async fn browser_close(
        &self,
        _context: RequestContext<RoleServer>,
        Parameters(args): Parameters<BrowserSessionArgs>,
    ) -> Result<String, String> {
        let audit_id = Store::new_audit_id();
        let now = Utc::now();
        let _ = self.camofox.close(&args.session_id).await;
        self.sessions.remove(&args.session_id);
        self.metrics
            .browser_sessions_active
            .set(self.sessions.count() as i64);
        self.metrics
            .tool_calls
            .with_label_values(&["browser_close", "ok"])
            .inc();
        let _ = self
            .store
            .write_audit(&AuditRecord {
                entry_id: audit_id.clone(),
                fetch_id: None,
                tool: "browser_close".into(),
                args_redacted: serde_json::to_value(&args).unwrap_or_default(),
                outcome: "session_close".into(),
                error: None,
                recorded_at: now,
            })
            .await;
        Ok(
            serde_json::to_string(&serde_json::json!({ "ok": true, "audit_id": audit_id }))
                .unwrap_or_default(),
        )
    }
}
