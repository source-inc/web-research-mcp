// crates/web-research-mcp/src/policy.rs
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use url::{Host, Url};

use crate::config::{Domains, Families, Limits};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Family {
    Search,
    Extract,
    Crawl,
    Browser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

#[derive(Clone)]
pub struct Policy {
    families: Families,
    limits: Limits,
    domains: Domains,
    inflight: Arc<AtomicUsize>,
}

impl Policy {
    pub fn new(families: Families, limits: Limits, domains: Domains) -> Self {
        Self {
            families,
            limits,
            domains,
            inflight: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn check_family(&self, fam: Family) -> Decision {
        let enabled = match fam {
            Family::Search => self.families.search,
            Family::Extract => self.families.extract,
            Family::Crawl => self.families.crawl,
            Family::Browser => self.families.browser,
        };
        if enabled {
            Decision::Allow
        } else {
            Decision::Deny(format!("family_disabled:{:?}", fam).to_lowercase())
        }
    }

    pub fn check_domain(&self, url_str: &str) -> Decision {
        let url = match Url::parse(url_str) {
            Ok(u) => u,
            Err(_) => return Decision::Deny("bad_url".to_string()),
        };
        if !matches!(url.scheme(), "http" | "https") {
            return Decision::Deny("unsupported_scheme".to_string());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Decision::Deny("url_credentials".to_string());
        }
        let host = match url.host_str() {
            Some(h) => h,
            None => return Decision::Deny("no_host".to_string()),
        };

        let parsed_ip = match url.host() {
            Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            _ => None,
        };
        if let Some(ip) = parsed_ip {
            if !is_public_ip(ip) {
                return Decision::Deny("non_public_ip".to_string());
            }
        }

        for pattern in &self.domains.denylist {
            if host_matches(pattern, host) {
                return Decision::Deny(format!("denylist:{pattern}"));
            }
        }
        if self.domains.allowlist.is_empty() {
            return Decision::Allow;
        }
        for pattern in &self.domains.allowlist {
            if host_matches(pattern, host) {
                return Decision::Allow;
            }
        }
        Decision::Deny("not_on_allowlist".to_string())
    }

    pub fn check_caps_search(&self, max_results: usize) -> Decision {
        if max_results == 0 || max_results > self.limits.search_max_results {
            return Decision::Deny(format!(
                "search_max_results:{}",
                self.limits.search_max_results
            ));
        }
        Decision::Allow
    }

    pub fn check_caps_crawl(&self, max_depth: usize, max_pages: usize) -> Decision {
        if max_depth == 0 || max_depth > self.limits.crawl_max_depth {
            return Decision::Deny(format!("crawl_max_depth:{}", self.limits.crawl_max_depth));
        }
        if max_pages == 0 || max_pages > self.limits.crawl_max_pages {
            return Decision::Deny(format!("crawl_max_pages:{}", self.limits.crawl_max_pages));
        }
        Decision::Allow
    }

    /// Acquire an inflight slot. Returns None if at cap.
    pub fn try_acquire_inflight(&self) -> Option<InflightGuard> {
        let max = self.limits.inflight_max;
        loop {
            let cur = self.inflight.load(Ordering::SeqCst);
            if cur >= max {
                return None;
            }
            if self
                .inflight
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Some(InflightGuard {
                    counter: Arc::clone(&self.inflight),
                });
            }
        }
    }
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_multicast()
                || ip.is_unspecified()
                || cidr_match("100.64.0.0/10", IpAddr::V4(ip)) == Some(true))
        }
        IpAddr::V6(ip) => {
            !(ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || cidr_match("fc00::/7", IpAddr::V6(ip)) == Some(true)
                || cidr_match("fe80::/10", IpAddr::V6(ip)) == Some(true)
                || cidr_match("2001:db8::/32", IpAddr::V6(ip)) == Some(true))
        }
    }
}

pub struct InflightGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    if pattern.contains('/') {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if let Some(matched) = cidr_match(pattern, ip) {
                return matched;
            }
        }
        return false;
    }
    host == pattern
}

fn cidr_match(pattern: &str, ip: IpAddr) -> Option<bool> {
    let (net_str, mask_str) = pattern.split_once('/')?;
    let mask: u8 = mask_str.parse().ok()?;
    let net: IpAddr = net_str.parse().ok()?;
    match (net, ip) {
        (IpAddr::V4(net), IpAddr::V4(ip)) => {
            if mask > 32 {
                return Some(false);
            }
            let shift = if mask == 0 { 32u32 } else { 32 - mask as u32 };
            let m = if shift == 32 { 0u32 } else { u32::MAX << shift };
            Some(u32::from(net) & m == u32::from(ip) & m)
        }
        (IpAddr::V6(net), IpAddr::V6(ip)) => {
            if mask > 128 {
                return Some(false);
            }
            let n = u128::from(net);
            let i = u128::from(ip);
            let shift = if mask == 0 { 128u32 } else { 128 - mask as u32 };
            let m = if shift == 128 {
                0u128
            } else {
                u128::MAX << shift
            };
            Some(n & m == i & m)
        }
        _ => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Policy {
        Policy::new(Families::default(), Limits::default(), Domains::default())
    }

    #[test]
    fn family_enabled_default() {
        assert_eq!(p().check_family(Family::Search), Decision::Allow);
    }

    #[test]
    fn family_disabled_denies() {
        let fams = Families {
            browser: false,
            ..Default::default()
        };
        let pol = Policy::new(fams, Limits::default(), Domains::default());
        assert!(matches!(
            pol.check_family(Family::Browser),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn denylist_blocks_fleet_internal() {
        let pol = p();
        assert!(matches!(
            pol.check_domain("http://localhost/x"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            pol.check_domain("http://service.internal/x"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            pol.check_domain("http://100.64.5.10/x"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            pol.check_domain("http://169.254.169.254/latest/meta-data"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            pol.check_domain("http://[::1]/x"),
            Decision::Deny(_)
        ));
        assert!(matches!(
            pol.check_domain("file:///etc/passwd"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn allowlist_empty_is_allow_by_default() {
        let pol = p();
        assert_eq!(pol.check_domain("https://example.com/"), Decision::Allow);
    }

    #[test]
    fn allowlist_restricts() {
        let dom = Domains {
            allowlist: vec!["example.com".into()],
            denylist: vec![],
        };
        let pol = Policy::new(Families::default(), Limits::default(), dom);
        assert_eq!(pol.check_domain("https://example.com/"), Decision::Allow);
        assert!(matches!(
            pol.check_domain("https://other.com/"),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn deny_wins_over_allow() {
        let dom = Domains {
            allowlist: vec!["*.example.com".into()],
            denylist: vec!["bad.example.com".into()],
        };
        let pol = Policy::new(Families::default(), Limits::default(), dom);
        assert!(matches!(
            pol.check_domain("https://bad.example.com/"),
            Decision::Deny(_)
        ));
        assert_eq!(pol.check_domain("https://ok.example.com/"), Decision::Allow);
    }

    #[test]
    fn wildcard_subdomain_match() {
        assert!(host_matches("*.example.com", "a.example.com"));
        assert!(host_matches("*.example.com", "example.com"));
        assert!(!host_matches("*.example.com", "other.com"));
    }

    #[test]
    fn cidr_match_ipv4() {
        assert!(host_matches("100.64.0.0/10", "100.64.5.5"));
        assert!(host_matches("100.64.0.0/10", "100.127.255.254"));
        assert!(!host_matches("100.64.0.0/10", "100.128.0.1"));
    }

    #[test]
    fn cap_search_results() {
        let pol = p();
        assert_eq!(pol.check_caps_search(10), Decision::Allow);
        assert!(matches!(pol.check_caps_search(0), Decision::Deny(_)));
        assert!(matches!(pol.check_caps_search(9999), Decision::Deny(_)));
    }

    #[test]
    fn cap_crawl() {
        let pol = p();
        assert_eq!(pol.check_caps_crawl(2, 50), Decision::Allow);
        assert!(matches!(pol.check_caps_crawl(99, 1), Decision::Deny(_)));
        assert!(matches!(pol.check_caps_crawl(1, 99999), Decision::Deny(_)));
    }

    #[test]
    fn inflight_caps_concurrent() {
        let lim = Limits {
            inflight_max: 2,
            ..Default::default()
        };
        let pol = Policy::new(
            Families::default(),
            lim,
            Domains {
                allowlist: vec![],
                denylist: vec![],
            },
        );
        let a = pol.try_acquire_inflight();
        let b = pol.try_acquire_inflight();
        let c = pol.try_acquire_inflight();
        assert!(a.is_some());
        assert!(b.is_some());
        assert!(c.is_none());
        drop(a);
        let d = pol.try_acquire_inflight();
        assert!(d.is_some());
    }
}
