use rmcp::handler::server::router::tool::ToolRouter;

use super::WebResearchMcp;

mod crawl;
mod evidence;
mod map;
mod scrape;

impl WebResearchMcp {
    pub(crate) fn fetch_router() -> ToolRouter<Self> {
        Self::scrape_router() + Self::evidence_router() + Self::map_router() + Self::crawl_router()
    }
}
