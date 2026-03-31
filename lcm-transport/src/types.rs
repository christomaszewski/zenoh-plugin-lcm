/// A fully reassembled LCM message.
#[derive(Debug, Clone)]
pub struct LcmMessage {
    /// The LCM channel name.
    pub channel: String,
    /// The LCM sequence number from the wire.
    pub sequence_number: u32,
    /// Raw payload: fingerprint (8 bytes) + serialized message data.
    pub data: Vec<u8>,
}

/// Parsed LCM multicast URL components.
#[derive(Debug, Clone)]
pub struct LcmUrl {
    pub multicast_group: std::net::Ipv4Addr,
    pub port: u16,
    pub ttl: u32,
    pub recv_buf_size: Option<usize>,
}

impl Default for LcmUrl {
    fn default() -> Self {
        Self {
            multicast_group: std::net::Ipv4Addr::new(239, 255, 76, 67),
            port: 7667,
            ttl: 0,
            recv_buf_size: None,
        }
    }
}

impl LcmUrl {
    /// Parse an LCM URL string of the form `udpm://GROUP:PORT?ttl=N&recv_buf_size=M`.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let rest = url
            .strip_prefix("udpm://")
            .ok_or_else(|| format!("LCM URL must start with 'udpm://': {url}"))?;

        // Split off query string
        let (host_port, query) = match rest.split_once('?') {
            Some((hp, q)) => (hp, Some(q)),
            None => (rest, None),
        };

        // Parse host:port
        let (group_str, port_str) = host_port
            .rsplit_once(':')
            .ok_or_else(|| format!("LCM URL must contain GROUP:PORT: {url}"))?;

        let multicast_group: std::net::Ipv4Addr = group_str
            .parse()
            .map_err(|e| format!("Invalid multicast group '{group_str}': {e}"))?;

        let port: u16 = port_str
            .parse()
            .map_err(|e| format!("Invalid port '{port_str}': {e}"))?;

        let mut ttl: u32 = 0;
        let mut recv_buf_size: Option<usize> = None;

        if let Some(query) = query {
            for param in query.split('&') {
                if let Some((key, value)) = param.split_once('=') {
                    match key {
                        "ttl" => {
                            ttl = value
                                .parse()
                                .map_err(|e| format!("Invalid ttl '{value}': {e}"))?;
                        }
                        "recv_buf_size" => {
                            recv_buf_size = Some(
                                value
                                    .parse()
                                    .map_err(|e| format!("Invalid recv_buf_size '{value}': {e}"))?,
                            );
                        }
                        _ => {
                            tracing::warn!("Unknown LCM URL parameter: {key}={value}");
                        }
                    }
                }
            }
        }

        Ok(Self {
            multicast_group,
            port,
            ttl,
            recv_buf_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_default_url() {
        let url = LcmUrl::parse("udpm://239.255.76.67:7667").unwrap();
        assert_eq!(url.multicast_group, std::net::Ipv4Addr::new(239, 255, 76, 67));
        assert_eq!(url.port, 7667);
        assert_eq!(url.ttl, 0);
        assert_eq!(url.recv_buf_size, None);
    }

    #[test]
    fn test_parse_url_with_params() {
        let url = LcmUrl::parse("udpm://239.255.76.67:7667?ttl=1&recv_buf_size=2097152").unwrap();
        assert_eq!(url.ttl, 1);
        assert_eq!(url.recv_buf_size, Some(2097152));
    }

    #[test]
    fn test_parse_invalid_scheme() {
        assert!(LcmUrl::parse("tcp://239.255.76.67:7667").is_err());
    }

    #[test]
    fn test_parse_empty_url() {
        assert!(LcmUrl::parse("").is_err());
    }

    #[test]
    fn test_parse_scheme_only() {
        assert!(LcmUrl::parse("udpm://").is_err());
    }

    #[test]
    fn test_parse_missing_port() {
        assert!(LcmUrl::parse("udpm://239.255.76.67").is_err());
    }

    #[test]
    fn test_parse_invalid_ip() {
        assert!(LcmUrl::parse("udpm://999.999.999.999:7667").is_err());
    }

    #[test]
    fn test_parse_invalid_port() {
        assert!(LcmUrl::parse("udpm://239.255.76.67:99999").is_err());
    }

    #[test]
    fn test_parse_non_numeric_port() {
        assert!(LcmUrl::parse("udpm://239.255.76.67:abc").is_err());
    }

    #[test]
    fn test_parse_invalid_ttl() {
        assert!(LcmUrl::parse("udpm://239.255.76.67:7667?ttl=not_a_number").is_err());
    }

    #[test]
    fn test_parse_invalid_recv_buf_size() {
        assert!(LcmUrl::parse("udpm://239.255.76.67:7667?recv_buf_size=xyz").is_err());
    }

    #[test]
    fn test_parse_high_ttl() {
        let url = LcmUrl::parse("udpm://239.255.76.67:7667?ttl=255").unwrap();
        assert_eq!(url.ttl, 255);
    }

    #[test]
    fn test_parse_port_zero() {
        let url = LcmUrl::parse("udpm://239.255.76.67:0").unwrap();
        assert_eq!(url.port, 0);
    }

    #[test]
    fn test_parse_whitespace_trimmed() {
        let url = LcmUrl::parse("  udpm://239.255.76.67:7667  ").unwrap();
        assert_eq!(url.port, 7667);
    }

    #[test]
    fn test_parse_unknown_param_ignored() {
        // Unknown params should not cause an error (just a warning log).
        let url = LcmUrl::parse("udpm://239.255.76.67:7667?ttl=1&unknown=42").unwrap();
        assert_eq!(url.ttl, 1);
    }

    #[test]
    fn test_parse_param_no_value() {
        // "ttl" without "=value" — split_once('=') returns None, so it's skipped.
        let url = LcmUrl::parse("udpm://239.255.76.67:7667?ttl").unwrap();
        assert_eq!(url.ttl, 0); // Default, since param wasn't parsed.
    }

    #[test]
    fn test_parse_large_recv_buf_size() {
        let url =
            LcmUrl::parse("udpm://239.255.76.67:7667?recv_buf_size=134217728").unwrap();
        assert_eq!(url.recv_buf_size, Some(134217728)); // 128 MB
    }

    #[test]
    fn test_default_url_values() {
        let url = LcmUrl::default();
        assert_eq!(url.multicast_group, std::net::Ipv4Addr::new(239, 255, 76, 67));
        assert_eq!(url.port, 7667);
        assert_eq!(url.ttl, 0);
        assert_eq!(url.recv_buf_size, None);
    }
}
