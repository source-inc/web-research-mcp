use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(
    name = "web-research-mcp",
    about = "Bounded web research MCP server orchestrating SearXNG, Firecrawl, and Camoufox"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print build and revision information.
    Version,
    /// Start the HTTP server with /version, /healthz, /metrics, /mcp.
    Serve,
    /// Run an end-to-end smoke check against a backend.
    Smoke {
        #[arg(long)]
        backend: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub data_dir: PathBuf,
    pub http_port: u16,
    pub http_bind_addr: String,
    pub mcp_path: String,
    pub backends: Backends,
    pub policy: Policy,
    pub store: Store,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Backends {
    pub searxng: BackendEndpoint,
    pub firecrawl: BackendEndpoint,
    pub camofox: BackendEndpoint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendEndpoint {
    pub endpoint: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub families: Families,
    pub limits: Limits,
    pub domains: Domains,
    pub browser: BrowserPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Families {
    pub search: bool,
    pub extract: bool,
    pub crawl: bool,
    pub browser: bool,
}

impl Default for Families {
    fn default() -> Self {
        Self {
            search: true,
            extract: true,
            // Start with the narrow research surface. Crawl and browser are
            // explicit operator opt-ins because they expand network reach and
            // resource consumption substantially.
            crawl: false,
            browser: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    pub search_max_results: usize,
    pub scrape_max_bytes: usize,
    pub crawl_max_depth: usize,
    pub crawl_max_pages: usize,
    pub browser_session_ttl_secs: u64,
    pub browser_session_idle_secs: u64,
    pub browser_max_concurrent: usize,
    pub inflight_max: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            search_max_results: 20,
            scrape_max_bytes: 2_000_000,
            crawl_max_depth: 2,
            crawl_max_pages: 50,
            browser_session_ttl_secs: 300,
            browser_session_idle_secs: 60,
            browser_max_concurrent: 4,
            inflight_max: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Domains {
    pub allowlist: Vec<String>,
    pub denylist: Vec<String>,
}

impl Default for Domains {
    fn default() -> Self {
        Self {
            allowlist: Vec::new(),
            denylist: vec![
                "*.internal".to_string(),
                "*.local".to_string(),
                "*.localhost".to_string(),
                "*.home.arpa".to_string(),
                "localhost".to_string(),
                "metadata.google.internal".to_string(),
                "0.0.0.0/8".to_string(),
                "10.0.0.0/8".to_string(),
                "127.0.0.1".to_string(),
                "169.254.0.0/16".to_string(),
                "172.16.0.0/12".to_string(),
                "192.0.0.0/24".to_string(),
                "192.168.0.0/16".to_string(),
                "100.64.0.0/10".to_string(),
                "224.0.0.0/4".to_string(),
                "240.0.0.0/4".to_string(),
                "::/128".to_string(),
                "::1/128".to_string(),
                "fc00::/7".to_string(),
                "fe80::/10".to_string(),
                "ff00::/8".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserPolicy {
    pub cookie_import_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub retention_days: u32,
}

impl Default for Store {
    fn default() -> Self {
        Self { retention_days: 30 }
    }
}

impl Default for Config {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            data_dir: home.join(".web-research-mcp").join("data"),
            http_port: 9213,
            http_bind_addr: "127.0.0.1".to_string(),
            mcp_path: "/mcp".to_string(),
            backends: Backends {
                searxng: BackendEndpoint {
                    endpoint: "http://127.0.0.1:9210".into(),
                    api_key: None,
                },
                firecrawl: BackendEndpoint {
                    endpoint: "http://127.0.0.1:9211".into(),
                    api_key: None,
                },
                camofox: BackendEndpoint {
                    endpoint: "http://127.0.0.1:9212".into(),
                    api_key: None,
                },
            },
            policy: Policy::default(),
            store: Store::default(),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        match path {
            Some(path) => {
                let contents = std::fs::read_to_string(path)
                    .with_context(|| format!("reading config from {}", path.display()))?;
                toml::from_str::<Self>(&contents)
                    .with_context(|| format!("parsing config from {}", path.display()))
            }
            None => Ok(Self::default()),
        }
    }

    pub fn default_config_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".web-research-mcp")
            .join("config.toml")
    }

    /// Apply environment variable overrides after loading from file (or defaults).
    /// Matches the pattern used by observability-mcp.
    pub fn apply_env_overrides(&mut self) {
        if let Ok(value) = std::env::var("WEB_RESEARCH_MCP_DATA_DIR") {
            self.data_dir = PathBuf::from(value);
        }
        if let Ok(value) = std::env::var("WEB_RESEARCH_MCP_HTTP_PORT") {
            if let Ok(port) = value.parse::<u16>() {
                self.http_port = port;
            }
        }
        if let Ok(value) = std::env::var("WEB_RESEARCH_MCP_HTTP_BIND_ADDR") {
            self.http_bind_addr = value;
        }
        if let Ok(value) = std::env::var("WEB_RESEARCH_MCP_MCP_PATH") {
            self.mcp_path = value;
        }
        if let Ok(value) = std::env::var("SEARXNG_ENDPOINT") {
            self.backends.searxng.endpoint = value;
        }
        if let Ok(value) = std::env::var("FIRECRAWL_ENDPOINT") {
            self.backends.firecrawl.endpoint = value;
        }
        if let Ok(value) = std::env::var("FIRECRAWL_API_KEY") {
            self.backends.firecrawl.api_key = Some(value);
        }
        if let Ok(value) = std::env::var("CAMOFOX_ENDPOINT") {
            self.backends.camofox.endpoint = value;
        }
        if let Ok(value) = std::env::var("CAMOFOX_API_KEY") {
            self.backends.camofox.api_key = Some(value);
        }
        apply_bool_env(
            "WEB_RESEARCH_MCP_ENABLE_SEARCH",
            &mut self.policy.families.search,
        );
        apply_bool_env(
            "WEB_RESEARCH_MCP_ENABLE_EXTRACT",
            &mut self.policy.families.extract,
        );
        apply_bool_env(
            "WEB_RESEARCH_MCP_ENABLE_CRAWL",
            &mut self.policy.families.crawl,
        );
        apply_bool_env(
            "WEB_RESEARCH_MCP_ENABLE_BROWSER",
            &mut self.policy.families.browser,
        );
    }
}

fn apply_bool_env(name: &str, target: &mut bool) {
    let Ok(value) = std::env::var(name) else {
        return;
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => *target = true,
        "0" | "false" | "no" | "off" => *target = false,
        _ => tracing::warn!(variable = name, value, "ignoring invalid boolean override"),
    }
}
