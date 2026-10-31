//! The `SITES` setting: which sites this Worker counts, and the browser
//! origins each one accepts hits and stats requests from.
//!
//! ```toml
//! [vars]
//! SITES = { portfolio = ["https://example.com", "https://*.example.com"] }
//! ```

use std::collections::HashMap;

pub struct Sites {
    sites: HashMap<String, Vec<OriginPattern>>,
}

/// An allowed origin. One `*` stands for a subdomain, so
/// `https://*.example.com` covers `https://blog.example.com`.
#[derive(Debug, PartialEq)]
enum OriginPattern {
    Exact(String),
    Wildcard { prefix: String, suffix: String },
}

impl Sites {
    pub fn from_map(map: HashMap<String, Vec<String>>) -> Result<Self, String> {
        if map.is_empty() {
            return Err("SITES lists no sites".into());
        }
        let mut sites = HashMap::with_capacity(map.len());
        for (site, origins) in map {
            if !is_valid_site_id(&site) {
                return Err(format!(
                    "site id {site:?} must be 1-64 letters, digits, '-' or '_'"
                ));
            }
            let patterns = origins
                .iter()
                .map(|origin| OriginPattern::parse(origin))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("site {site:?}: {e}"))?;
            sites.insert(site, patterns);
        }
        Ok(Self { sites })
    }

    /// Parses `SITES` given as a JSON string, the form `.dev.vars` and
    /// `wrangler dev --var` use.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let map =
            serde_json::from_str(json).map_err(|e| format!("SITES is not valid JSON: {e}"))?;
        Self::from_map(map)
    }

    pub fn contains(&self, site: &str) -> bool {
        self.sites.contains_key(site)
    }

    /// Whether `origin` (an `Origin` header) may count views for `site`.
    pub fn allows(&self, site: &str, origin: &str) -> bool {
        let origin = normalize(origin);
        self.sites
            .get(site)
            .is_some_and(|patterns| patterns.iter().any(|p| p.matches(&origin)))
    }

    /// Whether `origin` belongs to any site. CORS preflights don't carry
    /// the request body, so they can't say which site they are for.
    pub fn allows_any(&self, origin: &str) -> bool {
        let origin = normalize(origin);
        self.sites.values().flatten().any(|p| p.matches(&origin))
    }
}

impl OriginPattern {
    fn parse(raw: &str) -> Result<Self, String> {
        let origin = normalize(raw);
        let Some((scheme, host)) = origin.split_once("://") else {
            return Err(format!("origin {raw:?} needs a scheme, like https://"));
        };
        if scheme != "https" && scheme != "http" {
            return Err(format!("origin {raw:?} must use http or https"));
        }
        if host.is_empty() || host.contains('/') {
            return Err(format!("origin {raw:?} must not have a path"));
        }
        match origin.matches('*').count() {
            0 => Ok(Self::Exact(origin)),
            1 if host.starts_with("*.") => {
                let (prefix, suffix) = origin.split_once('*').unwrap();
                Ok(Self::Wildcard {
                    prefix: prefix.to_string(),
                    suffix: suffix.to_string(),
                })
            }
            _ => Err(format!(
                "origin {raw:?} can only use '*' as the first label, like https://*.example.com"
            )),
        }
    }

    fn matches(&self, origin: &str) -> bool {
        match self {
            Self::Exact(allowed) => allowed == origin,
            Self::Wildcard { prefix, suffix } => origin
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.strip_suffix(suffix.as_str()))
                .is_some_and(|sub| {
                    !sub.is_empty()
                        && sub
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
                }),
        }
    }
}

fn normalize(origin: &str) -> String {
    origin.trim().trim_end_matches('/').to_ascii_lowercase()
}

fn is_valid_site_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sites(json: &str) -> Sites {
        Sites::from_json(json).unwrap()
    }

    #[test]
    fn exact_origins() {
        let s = sites(r#"{"blog": ["https://example.com", "http://localhost:8080/"]}"#);
        assert!(s.allows("blog", "https://example.com"));
        assert!(s.allows("blog", "https://EXAMPLE.com"));
        assert!(s.allows("blog", "http://localhost:8080"));
        assert!(!s.allows("blog", "http://example.com"));
        assert!(!s.allows("blog", "https://example.com.evil.net"));
        assert!(!s.allows("blog", "http://localhost:3000"));
        assert!(!s.allows("blog", "null"));
        assert!(!s.allows("other", "https://example.com"));
    }

    #[test]
    fn wildcard_subdomains() {
        let s = sites(r#"{"blog": ["https://*.example.com"]}"#);
        assert!(s.allows("blog", "https://www.example.com"));
        assert!(s.allows("blog", "https://a.b.example.com"));
        assert!(!s.allows("blog", "https://example.com"));
        assert!(!s.allows("blog", "https://.example.com"));
        assert!(!s.allows("blog", "https://evil.com/.example.com"));
        assert!(!s.allows("blog", "https://evil.com:443.example.com"));
        assert!(!s.allows("blog", "https://www.example.com.evil.net"));
    }

    #[test]
    fn sites_are_kept_apart() {
        let s = sites(r#"{"a": ["https://a.dev"], "b": ["https://b.dev"]}"#);
        assert!(s.contains("a") && s.contains("b") && !s.contains("c"));
        assert!(s.allows("a", "https://a.dev"));
        assert!(!s.allows("a", "https://b.dev"));
        assert!(s.allows_any("https://b.dev"));
        assert!(!s.allows_any("https://c.dev"));
    }

    #[test]
    fn rejects_bad_config() {
        for bad in [
            r#"{}"#,
            r#"{"": ["https://a.dev"]}"#,
            r#"{"has space": ["https://a.dev"]}"#,
            r#"{"a": ["a.dev"]}"#,
            r#"{"a": ["ftp://a.dev"]}"#,
            r#"{"a": ["https://a.dev/blog"]}"#,
            r#"{"a": ["https://"]}"#,
            r#"{"a": ["https://www.*.dev"]}"#,
            r#"{"a": ["https://*.*.dev"]}"#,
            r#"{"a": "https://a.dev"}"#,
            r#"not json"#,
        ] {
            assert!(Sites::from_json(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn a_site_may_list_no_origins() {
        // Hits are refused, but badges and curl reads still work.
        let s = sites(r#"{"readme": []}"#);
        assert!(s.contains("readme"));
        assert!(!s.allows("readme", "https://a.dev"));
    }
}
