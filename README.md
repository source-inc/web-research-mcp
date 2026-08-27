# web-research-mcp

A small Rust MCP gateway for bounded, self-hosted web research. One binary
provides the policy, provenance, evidence-cache, metrics, and MCP boundary;
replaceable containers provide search, extraction, crawl, and browser
capabilities.

The service deliberately has no private repository, host, schema, or DefraDB
dependency.

## Why this shape

Deep research needs more than a generic `fetch` tool. It needs a repeatable
sequence:

1. search broadly and retain ranked candidate metadata;
2. fetch selected primary sources through a bounded extractor;
3. persist the exact evidence bytes with a stable ID and SHA-256 hash;
4. find and verify quotations against that stored evidence;
5. let the agent runtime persist claims, contradictions, and the final report.

`web-research-mcp` owns steps 1–4. An agent runtime such as
[Gents](https://github.com/source-inc/gents) owns the research graph and final
documents.

## Tool surface

| Family | Tools | Default |
| --- | --- | --- |
| Search | `web_search` | enabled |
| Extract | `web_scrape_url`, `web_map_site` | enabled |
| Evidence | `web_get_fetch`, `web_find_in_fetch`, `web_verify_quote` | enabled |
| Crawl | `web_crawl_site` | disabled |
| Browser | `browser_open`, `browser_snapshot`, actions, close | disabled |

Every successful scrape returns `fetch_id`, `content_hash`, final URL, byte
count, and a nonce-delimited untrusted-content envelope. Important quotations
should be checked with `web_verify_quote` immediately before citation.

## Quick start

Run the deterministic, internet-free contract stack:

```sh
tests/docker-contract.sh
```

It builds the real gateway plus a tiny SearXNG/Firecrawl-compatible fixture,
then verifies health, MCP initialization, tool discovery, search, scrape,
provenance, and prompt-injection marker neutralization. This is the fixture
intended for integration tests in downstream runtimes.

For a local binary:

```sh
cargo run -- serve
```

The default backend endpoints are:

- SearXNG: `http://127.0.0.1:9210`
- Firecrawl: `http://127.0.0.1:9211`
- Camoufox: `http://127.0.0.1:9212`
- MCP gateway: `http://127.0.0.1:9213/mcp`

## Configuration

Pass `--config path/to/config.toml` or use
`~/.web-research-mcp/config.toml`. Environment overrides cover the common
container deployment case:

| Variable | Purpose |
| --- | --- |
| `WEB_RESEARCH_MCP_HTTP_BIND_ADDR` | HTTP bind address |
| `WEB_RESEARCH_MCP_HTTP_PORT` | HTTP port |
| `WEB_RESEARCH_MCP_DATA_DIR` | Fetch/audit evidence directory |
| `WEB_RESEARCH_MCP_MCP_PATH` | MCP path, default `/mcp` |
| `SEARXNG_ENDPOINT` | SearXNG base URL |
| `FIRECRAWL_ENDPOINT` | Firecrawl-compatible base URL |
| `FIRECRAWL_API_KEY` | Optional bearer token |
| `CAMOFOX_ENDPOINT` | Camoufox-compatible base URL |
| `CAMOFOX_API_KEY` | Optional bearer token |
| `WEB_RESEARCH_MCP_ENABLE_CRAWL` | Opt into crawl tools |
| `WEB_RESEARCH_MCP_ENABLE_BROWSER` | Opt into browser tools |

The TOML policy also controls result/page/byte/concurrency caps and domain
allow/deny lists. Search and extract are enabled by default; crawl and browser
must be explicitly enabled.

## Security boundary

The service rejects non-HTTP schemes, URL credentials, private/special-use IP
literals, and configured internal domains before backend calls. Evidence is
wrapped with unpredictable nonce markers and common spoofed control tags are
neutralized.

That is defense in depth, not a complete network sandbox. Production extraction
and browser containers should also run on an egress-controlled network and
independently reject private IPs after DNS resolution and on every redirect.
Do not expose unauthenticated backend ports publicly.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
tests/docker-contract.sh
```

The product stays one Rust binary. `mock-web-backends` is a deterministic test
fixture built only for the contract stack.

## License

Apache-2.0.
