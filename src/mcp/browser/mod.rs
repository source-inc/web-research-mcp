use rmcp::handler::server::router::tool::ToolRouter;

use super::WebResearchMcp;

mod actions;
mod cookies;
mod session;

impl WebResearchMcp {
    pub(crate) fn browser_router() -> ToolRouter<Self> {
        Self::browser_session_router()
            + Self::browser_actions_router()
            + Self::browser_cookies_router()
    }
}
