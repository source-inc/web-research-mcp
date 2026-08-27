# AGENTS.md — web-research-mcp

Portable, bounded web-research MCP service. It coordinates SearXNG search,
Firecrawl-compatible extraction/crawl, and a Camoufox-compatible browser API.
The HTTP surface exposes `/healthz`, `/version`, `/metrics`, and `/mcp`.

## Work Here

- Service wiring: `src/mcp/mod.rs`
- Tool argument/result types: `src/mcp/domain.rs`
- Search tools: `src/mcp/search.rs`
- Fetch, evidence, and crawl tools: `src/mcp/fetch/`
- Browser tools: `src/mcp/browser/`
- Backend clients: `src/backends/`
- Real backend deployment examples: maintained by consuming runtimes

## Local Checks

- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- downstream live acceptance against real SearXNG, Firecrawl, and a real model

## Boundaries

- This repository owns the portable service, image, backend contracts, and
  full-stack examples.
- Gents owns agent graph documents, requests, responses, and research output.
- Private deployments own host inventory, credentials, and release pins.
- Search and extract are enabled by default. Crawl and browser are explicit
  operator opt-ins.
- Do not add mock backend services or claim a mock-backed run as integration
  acceptance.
- Never add private hostnames, credentials, deployment-specific paths, or
  private Git dependencies here.
