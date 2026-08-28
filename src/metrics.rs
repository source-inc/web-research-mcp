// crates/web-research-mcp/src/metrics.rs
use anyhow::Result;
use prometheus::{
    histogram_opts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};

#[derive(Clone)]
pub struct Metrics {
    pub registry: Registry,
    pub tool_calls: IntCounterVec,
    pub backend_latency: HistogramVec,
    pub browser_sessions_active: IntGauge,
    pub policy_denials: IntCounterVec,
    pub search_engine_events: IntCounterVec,
    pub evidence_candidates: IntCounterVec,
    pub evidence_bundle_duration: HistogramVec,
}

impl Metrics {
    pub fn new() -> Result<Self> {
        let registry = Registry::new();
        let tool_calls = IntCounterVec::new(
            Opts::new(
                "web_research_tool_calls_total",
                "Tool calls by tool and outcome",
            ),
            &["tool", "outcome"],
        )?;
        let backend_latency = HistogramVec::new(
            histogram_opts!(
                "web_research_backend_latency_seconds",
                "Backend call latency by backend + op"
            ),
            &["backend", "op"],
        )?;
        let browser_sessions_active = IntGauge::new(
            "web_research_browser_sessions_active",
            "Number of currently open browser sessions",
        )?;
        let policy_denials = IntCounterVec::new(
            Opts::new(
                "web_research_policy_denials_total",
                "Policy denials by reason",
            ),
            &["reason"],
        )?;
        let search_engine_events = IntCounterVec::new(
            Opts::new(
                "web_research_search_engine_events_total",
                "Search engine contribution and failure events",
            ),
            &["engine", "outcome"],
        )?;
        let evidence_candidates = IntCounterVec::new(
            Opts::new(
                "web_research_evidence_candidates_total",
                "Evidence candidates by pipeline stage and outcome",
            ),
            &["stage", "outcome"],
        )?;
        let evidence_bundle_duration = HistogramVec::new(
            histogram_opts!(
                "web_research_evidence_bundle_duration_seconds",
                "End-to-end evidence bundle duration by outcome"
            ),
            &["outcome"],
        )?;
        registry.register(Box::new(tool_calls.clone()))?;
        registry.register(Box::new(backend_latency.clone()))?;
        registry.register(Box::new(browser_sessions_active.clone()))?;
        registry.register(Box::new(policy_denials.clone()))?;
        registry.register(Box::new(search_engine_events.clone()))?;
        registry.register(Box::new(evidence_candidates.clone()))?;
        registry.register(Box::new(evidence_bundle_duration.clone()))?;
        Ok(Self {
            registry,
            tool_calls,
            backend_latency,
            browser_sessions_active,
            policy_denials,
            search_engine_events,
            evidence_candidates,
            evidence_bundle_duration,
        })
    }

    pub fn encode(&self) -> Result<String> {
        let mut buf = String::new();
        TextEncoder::new().encode_utf8(&self.registry.gather(), &mut buf)?;
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_register_and_increment() {
        let m = Metrics::new().unwrap();
        m.tool_calls.with_label_values(&["web_search", "ok"]).inc();
        m.policy_denials
            .with_label_values(&["denylist:*.internal"])
            .inc();
        m.browser_sessions_active.set(2);
        let body = m.encode().unwrap();
        assert!(body.contains("web_research_tool_calls_total"));
        assert!(body.contains("web_research_policy_denials_total"));
        assert!(body.contains("web_research_browser_sessions_active"));
    }
}
