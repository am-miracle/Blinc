//! Deep linking — cross-platform URI handling
//!
//! Platform implementations register a deep link handler that the router
//! calls when the app receives a URI from the OS (Android intent, iOS
//! universal link, desktop CLI argument, etc.)

/// Source of a deep link
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeepLinkSource {
    /// OS-level (Android intent, iOS universal link)
    System,
    /// In-app programmatic
    Internal,
    /// Push notification payload
    Push,
}

/// Parsed deep link URI
#[derive(Clone, Debug)]
pub struct DeepLink {
    pub scheme: String,
    pub host: Option<String>,
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
    pub source: DeepLinkSource,
}

impl DeepLink {
    /// Parse a URI string
    pub fn parse(uri: &str, source: DeepLinkSource) -> Option<Self> {
        // Simple URI parser: scheme://host/path?query#fragment
        let (scheme, rest) = uri.split_once("://")?;
        let (rest, fragment) = match rest.rsplit_once('#') {
            Some((r, f)) => (r, Some(f.to_string())),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(q.to_string())),
            None => (rest, None),
        };
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (Some(h.to_string()), format!("/{}", p)),
            None => {
                if rest.is_empty() {
                    (None, "/".to_string())
                } else {
                    (Some(rest.to_string()), "/".to_string())
                }
            }
        };

        Some(Self {
            scheme: scheme.to_string(),
            host,
            path,
            query,
            fragment,
            source,
        })
    }

    /// Get the path suitable for router navigation
    pub fn route_path(&self) -> String {
        match &self.query {
            Some(q) => format!("{}?{}", self.path, q),
            None => self.path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_full_uri() {
        let dl = DeepLink::parse(
            "myapp://example.com/users/42?tab=posts#section",
            DeepLinkSource::System,
        )
        .unwrap();
        assert_eq!(dl.scheme, "myapp");
        assert_eq!(dl.host.as_deref(), Some("example.com"));
        assert_eq!(dl.path, "/users/42");
        assert_eq!(dl.query.as_deref(), Some("tab=posts"));
        assert_eq!(dl.fragment.as_deref(), Some("section"));
        assert_eq!(dl.route_path(), "/users/42?tab=posts");
    }

    #[test]
    fn test_parse_simple_uri() {
        let dl = DeepLink::parse("https://app/settings", DeepLinkSource::Internal).unwrap();
        assert_eq!(dl.path, "/settings");
        assert_eq!(dl.host.as_deref(), Some("app"));
    }

    #[test]
    fn test_parse_root() {
        let dl = DeepLink::parse("myapp://", DeepLinkSource::System).unwrap();
        assert_eq!(dl.path, "/");
        assert!(dl.host.is_none());
    }
}
