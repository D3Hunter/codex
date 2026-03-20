use std::net::SocketAddr;
use std::str::FromStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpListenTransport {
    Stdio,
    Http {
        bind_address: SocketAddr,
        path: String,
    },
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum McpListenTransportParseError {
    UnsupportedListenUrl(String),
    InvalidStdioListenUrl(String),
    InvalidHttpListenUrl {
        listen_url: String,
        reason: &'static str,
    },
}

impl std::fmt::Display for McpListenTransportParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpListenTransportParseError::UnsupportedListenUrl(listen_url) => write!(
                f,
                "unsupported --listen URL `{listen_url}`; expected `stdio://` or `http://IP:PORT[/PATH]`"
            ),
            McpListenTransportParseError::InvalidStdioListenUrl(listen_url) => write!(
                f,
                "invalid stdio --listen URL `{listen_url}`; expected exactly `stdio://`"
            ),
            McpListenTransportParseError::InvalidHttpListenUrl { listen_url, reason } => write!(
                f,
                "invalid HTTP --listen URL `{listen_url}`: {reason}; expected `http://IP:PORT[/PATH]`"
            ),
        }
    }
}

impl std::error::Error for McpListenTransportParseError {}

impl McpListenTransport {
    pub const DEFAULT_LISTEN_URL: &'static str = "stdio://";
    pub const DEFAULT_HTTP_PATH: &'static str = "/mcp";

    pub fn from_listen_url(listen_url: &str) -> Result<Self, McpListenTransportParseError> {
        if listen_url == Self::DEFAULT_LISTEN_URL {
            return Ok(Self::Stdio);
        }

        if listen_url.starts_with("stdio://") {
            return Err(McpListenTransportParseError::InvalidStdioListenUrl(
                listen_url.to_string(),
            ));
        }

        if let Some(http_url) = listen_url.strip_prefix("http://") {
            return Self::parse_http_listen_url(listen_url, http_url);
        }

        Err(McpListenTransportParseError::UnsupportedListenUrl(
            listen_url.to_string(),
        ))
    }

    fn parse_http_listen_url(
        listen_url: &str,
        http_url: &str,
    ) -> Result<Self, McpListenTransportParseError> {
        if http_url.contains('?') || http_url.contains('#') {
            return Err(McpListenTransportParseError::InvalidHttpListenUrl {
                listen_url: listen_url.to_string(),
                reason: "query strings and fragments are not supported",
            });
        }

        let (authority, path) = match http_url.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (http_url, Self::DEFAULT_HTTP_PATH.to_string()),
        };

        let bind_address = authority.parse::<SocketAddr>().map_err(|_| {
            McpListenTransportParseError::InvalidHttpListenUrl {
                listen_url: listen_url.to_string(),
                reason: "host must be an IP literal with an explicit port",
            }
        })?;

        Ok(Self::Http { bind_address, path })
    }
}

impl FromStr for McpListenTransport {
    type Err = McpListenTransportParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_listen_url(s)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::McpListenTransport;

    #[test]
    fn mcp_listen_transport_parses_stdio_listen_url() {
        let transport = McpListenTransport::from_listen_url(McpListenTransport::DEFAULT_LISTEN_URL)
            .expect("stdio listen URL should parse");
        assert_eq!(transport, McpListenTransport::Stdio);
    }

    #[test]
    fn mcp_listen_transport_parses_http_listen_url_with_default_path() {
        let transport = McpListenTransport::from_listen_url("http://127.0.0.1:8080")
            .expect("http listen URL should parse");
        assert_eq!(
            transport,
            McpListenTransport::Http {
                bind_address: "127.0.0.1:8080".parse().expect("valid socket address"),
                path: "/mcp".to_string(),
            }
        );
    }

    #[test]
    fn mcp_listen_transport_parses_http_listen_url_with_custom_path() {
        let transport = McpListenTransport::from_listen_url("http://127.0.0.1:8080/custom")
            .expect("http listen URL with path should parse");
        assert_eq!(
            transport,
            McpListenTransport::Http {
                bind_address: "127.0.0.1:8080".parse().expect("valid socket address"),
                path: "/custom".to_string(),
            }
        );
    }

    #[test]
    fn mcp_listen_transport_parses_ipv6_http_listen_url() {
        let transport = McpListenTransport::from_listen_url("http://[::1]:8080/mcp")
            .expect("ipv6 http listen URL should parse");
        assert_eq!(
            transport,
            McpListenTransport::Http {
                bind_address: "[::1]:8080".parse().expect("valid socket address"),
                path: "/mcp".to_string(),
            }
        );
    }

    #[test]
    fn mcp_listen_transport_rejects_invalid_stdio_listen_url() {
        let err = McpListenTransport::from_listen_url("stdio://extra")
            .expect_err("stdio listen URL with extra components should fail");
        assert_eq!(
            err.to_string(),
            "invalid stdio --listen URL `stdio://extra`; expected exactly `stdio://`"
        );
    }

    #[test]
    fn mcp_listen_transport_rejects_http_listen_url_without_ip_literal_host() {
        let err = McpListenTransport::from_listen_url("http://localhost:8080")
            .expect_err("hostname bind address should be rejected");
        assert_eq!(
            err.to_string(),
            "invalid HTTP --listen URL `http://localhost:8080`: host must be an IP literal with an explicit port; expected `http://IP:PORT[/PATH]`"
        );
    }

    #[test]
    fn mcp_listen_transport_rejects_http_listen_url_with_query() {
        let err = McpListenTransport::from_listen_url("http://127.0.0.1:8080/mcp?foo=bar")
            .expect_err("query parameters should be rejected");
        assert_eq!(
            err.to_string(),
            "invalid HTTP --listen URL `http://127.0.0.1:8080/mcp?foo=bar`: query strings and fragments are not supported; expected `http://IP:PORT[/PATH]`"
        );
    }

    #[test]
    fn mcp_listen_transport_rejects_http_listen_url_without_port() {
        let err = McpListenTransport::from_listen_url("http://127.0.0.1")
            .expect_err("missing port should be rejected");
        assert_eq!(
            err.to_string(),
            "invalid HTTP --listen URL `http://127.0.0.1`: host must be an IP literal with an explicit port; expected `http://IP:PORT[/PATH]`"
        );
    }

    #[test]
    fn mcp_listen_transport_rejects_unsupported_listen_url() {
        let err = McpListenTransport::from_listen_url("https://127.0.0.1:8080")
            .expect_err("unsupported scheme should fail");
        assert_eq!(
            err.to_string(),
            "unsupported --listen URL `https://127.0.0.1:8080`; expected `stdio://` or `http://IP:PORT[/PATH]`"
        );
    }
}
