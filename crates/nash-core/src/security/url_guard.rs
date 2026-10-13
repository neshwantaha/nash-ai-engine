//! URL / destination security policy (SSRF protection).
//!
//! Every outbound request built by the engine passes through
//! [`SecurityPolicy::validate_url`] *and* [`SecurityPolicy::validate_ips`]
//! (after DNS resolution, and again after each redirect). This defends against
//! SSRF, DNS-rebinding, and accidental plaintext credentials to a remote host.

use crate::error::{EngineError, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Url;

/// How strict destination validation should be.
#[derive(Debug, Clone)]
pub struct SecurityPolicy {
    /// Permit `http://` to non-loopback hosts. Off by default.
    pub allow_insecure_http: bool,
    /// Permit requests to private / link-local / loopback ranges other than
    /// loopback. Off by default (loopback is always allowed so local servers
    /// such as Ollama / LM Studio work out of the box).
    pub allow_private_networks: bool,
    /// Additional host allowlist. When non-empty, only these hosts (plus the
    /// configured provider base URLs) may be contacted by the *agent* web tool.
    pub allowed_hosts: Vec<String>,
    /// Always block cloud metadata endpoints. Should never be disabled.
    pub block_metadata_endpoints: bool,
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        Self {
            allow_insecure_http: false,
            allow_private_networks: false,
            allowed_hosts: Vec::new(),
            block_metadata_endpoints: true,
        }
    }
}

impl SecurityPolicy {
    /// A permissive policy for tests and explicitly trusted local development.
    pub fn permissive() -> Self {
        Self {
            allow_insecure_http: true,
            allow_private_networks: true,
            allowed_hosts: Vec::new(),
            block_metadata_endpoints: true,
        }
    }

    /// Validate the static parts of a URL: scheme and (for literal IP hosts or
    /// well-known metadata names) the destination.
    pub fn validate_url(&self, url: &Url) -> Result<()> {
        match url.scheme() {
            "https" => {}
            "http" => {
                let host = url.host_str().unwrap_or("");
                let local = is_loopback_host(host) || self.allow_insecure_http;
                if !local {
                    return Err(EngineError::Security(format!(
                        "plaintext http:// to non-local host '{host}' is blocked; use https:// or enable allow_insecure_http"
                    )));
                }
            }
            other => {
                return Err(EngineError::Security(format!(
                    "unsupported URL scheme '{other}' (only http/https are permitted)"
                )));
            }
        }

        let host = url
            .host_str()
            .ok_or_else(|| EngineError::Security("URL has no host".into()))?;

        if self.block_metadata_endpoints && is_metadata_host(host) {
            return Err(EngineError::Security(format!(
                "blocked cloud metadata endpoint '{host}'"
            )));
        }

        // If the host is a literal IP, classify it immediately.
        if let Ok(ip) = host.parse::<IpAddr>() {
            self.validate_ips(&[ip])?;
        } else if is_metadata_host(host) {
            // handled above
        }

        Ok(())
    }

    /// Validate every resolved IP for a destination.
    pub fn validate_ips(&self, ips: &[IpAddr]) -> Result<()> {
        for ip in ips {
            if self.block_metadata_endpoints && is_metadata_ip(*ip) {
                return Err(EngineError::Security(format!(
                    "blocked metadata service address {ip}"
                )));
            }
            if ip.is_loopback() {
                continue; // local servers are allowed
            }
            if is_private_ip(*ip) && !self.allow_private_networks {
                return Err(EngineError::Security(format!(
                    "requests to private-network address {ip} are blocked (enable allow_private_networks to permit)"
                )));
            }
        }
        Ok(())
    }

    /// Resolve `url`'s host and validate all resulting addresses. Performs the
    /// DNS lookup that closes the DNS-rebinding window.
    pub async fn resolve_and_validate(&self, url: &Url) -> Result<()> {
        self.validate_url(url)?;
        let host = url
            .host_str()
            .ok_or_else(|| EngineError::Security("URL has no host".into()))?;
        // Literal IP: already validated in validate_url.
        if host.parse::<IpAddr>().is_ok() {
            return Ok(());
        }
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| EngineError::Network(format!("DNS resolution failed for '{host}': {e}")))?;
        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        if ips.is_empty() {
            return Err(EngineError::Network(format!("no addresses resolved for '{host}'")));
        }
        self.validate_ips(&ips)
    }

    /// Check an agent-tool host against an optional allowlist.
    pub fn check_allowlisted(&self, host: &str) -> Result<()> {
        if self.allowed_hosts.is_empty() {
            return Ok(());
        }
        let host = host.to_ascii_lowercase();
        if self
            .allowed_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&host) || host.ends_with(&format!(".{h}")))
        {
            Ok(())
        } else {
            Err(EngineError::Security(format!(
                "host '{host}' is not in the configured allowlist"
            )))
        }
    }
}

/// True for `localhost`, `*.localhost`, or any loopback literal.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost"
        || h.ends_with(".localhost")
        || h == "[::1]"
        || h == "::1"
        || h.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

/// Known cloud metadata hostnames.
pub fn is_metadata_host(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    matches!(
        h.as_str(),
        "metadata.google.internal"
            | "metadata"
            | "metadata.goog"
            | "instance-data"
    )
}

/// Known cloud metadata IP addresses.
pub fn is_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4 == Ipv4Addr::new(169, 254, 169, 254)
            || v4 == Ipv4Addr::new(169, 254, 170, 2)
            || v4.octets()[0] == 100 && v4.octets()[1] == 100 && v4.octets()[2] == 100,
        IpAddr::V6(v6) => v6 == "fd00:ec2::254".parse::<Ipv6Addr>().unwrap(),
    }
}

/// RFC1918 + link-local + CGNAT + unspecified.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            v6.is_unspecified()
                || (seg & 0xfe00) == 0xfc00
                || (seg & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn https_public_ok() {
        let p = SecurityPolicy::default();
        assert!(p.validate_url(&url("https://api.openai.com/v1/chat")).is_ok());
    }

    #[test]
    fn plaintext_remote_blocked() {
        let p = SecurityPolicy::default();
        assert!(p.validate_url(&url("http://api.example.com/v1")).is_err());
    }

    #[test]
    fn loopback_http_allowed() {
        let p = SecurityPolicy::default();
        assert!(p.validate_url(&url("http://localhost:11434/api/chat")).is_ok());
        assert!(p.validate_url(&url("http://127.0.0.1:1234/v1")).is_ok());
    }

    #[test]
    fn metadata_endpoints_blocked() {
        let p = SecurityPolicy::permissive();
        assert!(p.validate_url(&url("http://169.254.169.254/latest/meta-data")).is_err());
        assert!(p.validate_url(&url("https://metadata.google.internal/x")).is_err());
    }

    #[test]
    fn private_ip_blocked_unless_permitted() {
        let strict = SecurityPolicy::default();
        let ip: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(strict.validate_ips(&[ip]).is_err());

        let relaxed = SecurityPolicy::permissive();
        assert!(relaxed.validate_ips(&[ip]).is_ok());
    }

    #[test]
    fn loopback_ip_always_allowed() {
        let p = SecurityPolicy::default();
        assert!(p.validate_ips(&["127.0.0.1".parse().unwrap()]).is_ok());
        assert!(p.validate_ips(&["::1".parse().unwrap()]).is_ok());
    }

    #[test]
    fn metadata_ip_always_blocked() {
        let p = SecurityPolicy::permissive();
        assert!(p.validate_ips(&["169.254.169.254".parse().unwrap()]).is_err());
    }

    #[test]
    fn allowlist_enforced() {
        let p = SecurityPolicy {
            allowed_hosts: vec!["example.com".into()],
            ..Default::default()
        };
        assert!(p.check_allowlisted("example.com").is_ok());
        assert!(p.check_allowlisted("sub.example.com").is_ok());
        assert!(p.check_allowlisted("evil.com").is_err());
    }
}
