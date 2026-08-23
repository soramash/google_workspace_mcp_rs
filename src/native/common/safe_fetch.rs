//! Bounded HTTP fetching with DNS-rebinding and SSRF defenses.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use reqwest::header::{CONTENT_LENGTH, HeaderMap, LOCATION};
use reqwest::{StatusCode, Url};

pub const DEFAULT_MAX_REDIRECTS: usize = 10;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A fully buffered response whose body was bounded while being read.
#[derive(Debug)]
pub struct FetchedResponse {
    pub final_url: Url,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// SSRF-safe fetch policy. The byte ceiling is supplied by each caller.
#[derive(Clone, Copy, Debug)]
pub struct SafeFetcher {
    max_bytes: usize,
    timeout: Duration,
    max_redirects: usize,
}

impl SafeFetcher {
    pub fn new(max_bytes: usize, timeout: Duration) -> Self {
        Self {
            max_bytes,
            timeout,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }

    pub fn with_max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    /// Fetch an HTTP(S) URL, validating and pinning every redirect hop.
    pub async fn fetch(&self, url: &str) -> Result<FetchedResponse, String> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(|| String::from("Safe fetch timeout is too large"))?;
        let mut current = Url::parse(url).map_err(|_| String::from("Invalid URL"))?;
        for redirect_count in 0..=self.max_redirects {
            validate_http_url(&current)?;
            let host = current
                .host_str()
                .ok_or_else(|| String::from("Invalid URL: missing hostname"))?
                .to_owned();
            let port = current
                .port_or_known_default()
                .ok_or_else(|| String::from("Invalid URL: missing port"))?;
            let dns_timeout = remaining_timeout(deadline)?;
            let addresses =
                tokio::time::timeout(dns_timeout, resolve_and_validate_host(&host, port))
                    .await
                    .map_err(|_| String::from("Safe fetch timed out during DNS resolution"))??;
            let request_timeout = remaining_timeout(deadline)?;

            // The request URL retains the original hostname, so reqwest emits that
            // Host header and uses it for TLS SNI. Only name resolution is replaced
            // with the complete set of addresses validated immediately above.
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(request_timeout.min(Duration::from_secs(5)))
                .timeout(request_timeout)
                .resolve_to_addrs(&host, &addresses)
                .build()
                .map_err(|_| String::from("Failed to initialize safe HTTP client"))?;
            let mut response = client
                .get(current.clone())
                .send()
                .await
                .map_err(|_| format!("Failed to fetch {}", redact_url(&current)))?;

            if is_redirect(response.status()) {
                if redirect_count == self.max_redirects {
                    return Err(format!(
                        "Too many redirects (max {}) fetching {}",
                        self.max_redirects,
                        redact_url(&current)
                    ));
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        format!(
                            "Redirect with no valid Location header from {}",
                            redact_url(&current)
                        )
                    })?;
                current = current
                    .join(location)
                    .map_err(|_| String::from("Redirect URL is invalid"))?;
                continue;
            }

            if declared_length(response.headers()).is_some_and(|length| length > self.max_bytes) {
                return Err(format!(
                    "Response exceeds the {} byte limit",
                    self.max_bytes
                ));
            }
            let status = response.status();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| format!("Failed to read response from {}", redact_url(&current)))?
            {
                if body.len().saturating_add(chunk.len()) > self.max_bytes {
                    return Err(format!(
                        "Response exceeds the {} byte limit",
                        self.max_bytes
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(FetchedResponse {
                final_url: current,
                status,
                headers,
                body,
            });
        }
        Err(String::from("Safe fetch could not complete"))
    }
}

pub async fn fetch_url(
    url: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<FetchedResponse, String> {
    SafeFetcher::new(max_bytes, timeout).fetch(url).await
}

/// Resolve all A/AAAA answers and fail closed if any answer is denied.
pub async fn resolve_and_validate_host(
    hostname: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, String> {
    if hostname.is_empty() {
        return Err(String::from("Invalid URL: no hostname"));
    }
    let lookup_host = hostname.to_owned();
    let display_host = hostname.to_owned();
    let resolved = tokio::task::spawn_blocking(move || {
        (lookup_host.as_str(), port)
            .to_socket_addrs()
            .map(|addresses| addresses.collect::<Vec<_>>())
    })
    .await
    .map_err(|_| format!("Cannot resolve hostname '{display_host}'"))?
    .map_err(|error| {
        format!(
            "Cannot resolve hostname '{display_host}': {error}. Refusing request (fail-closed)."
        )
    })?;
    if resolved.is_empty() {
        return Err(format!("No addresses found for hostname: {display_host}"));
    }

    let mut seen = HashSet::new();
    let mut validated = Vec::new();
    for address in resolved {
        if let Some(reason) = blocked_ip_reason(address.ip()) {
            return Err(format!(
                "URLs pointing to private/internal networks are not allowed: {display_host} resolves to {reason}"
            ));
        }
        if seen.insert(address) {
            validated.push(address);
        }
    }
    Ok(validated)
}

/// Explain why an IP cannot be contacted, or return `None` for an allowed address.
pub fn blocked_ip_reason(ip: IpAddr) -> Option<String> {
    match ip {
        IpAddr::V4(ipv4) => blocked_ipv4(ipv4),
        IpAddr::V6(ipv6) => {
            if let Some(mapped) = ipv6.to_ipv4_mapped() {
                blocked_ipv4(mapped).map(|reason| format!("{ipv6} embeds {mapped}, {reason}"))
            } else {
                blocked_ipv6(ipv6)
            }
        }
    }
}

fn blocked_ipv4(ip: Ipv4Addr) -> Option<String> {
    const NETWORKS: [(&str, u32, u8); 15] = [
        ("0.0.0.0/8", 0x0000_0000, 8),
        ("10.0.0.0/8", 0x0a00_0000, 8),
        ("100.64.0.0/10", 0x6440_0000, 10),
        ("127.0.0.0/8", 0x7f00_0000, 8),
        ("169.254.0.0/16", 0xa9fe_0000, 16),
        ("172.16.0.0/12", 0xac10_0000, 12),
        ("192.0.0.0/24", 0xc000_0000, 24),
        ("192.0.2.0/24", 0xc000_0200, 24),
        ("192.88.99.0/24", 0xc058_6300, 24),
        ("192.168.0.0/16", 0xc0a8_0000, 16),
        ("198.18.0.0/15", 0xc612_0000, 15),
        ("198.51.100.0/24", 0xc633_6400, 24),
        ("203.0.113.0/24", 0xcb00_7100, 24),
        ("224.0.0.0/4", 0xe000_0000, 4),
        ("240.0.0.0/4", 0xf000_0000, 4),
    ];
    let value = u32::from(ip);
    NETWORKS.iter().find_map(|(label, network, prefix)| {
        in_cidr_v4(value, *network, *prefix)
            .then(|| format!("{ip} is in the blocked range {label}"))
    })
}

fn blocked_ipv6(ip: Ipv6Addr) -> Option<String> {
    const NETWORKS: [(&str, u128, u8); 13] = [
        ("::/96", 0, 96),
        (
            "64:ff9b::/96",
            0x0064_ff9b_0000_0000_0000_0000_0000_0000,
            96,
        ),
        (
            "64:ff9b:1::/48",
            0x0064_ff9b_0001_0000_0000_0000_0000_0000,
            48,
        ),
        ("100::/64", 0x0100_0000_0000_0000_0000_0000_0000_0000, 64),
        ("2001::/23", 0x2001_0000_0000_0000_0000_0000_0000_0000, 23),
        (
            "2001:db8::/32",
            0x2001_0db8_0000_0000_0000_0000_0000_0000,
            32,
        ),
        ("2002::/16", 0x2002_0000_0000_0000_0000_0000_0000_0000, 16),
        ("3fff::/20", 0x3fff_0000_0000_0000_0000_0000_0000_0000, 20),
        ("5f00::/16", 0x5f00_0000_0000_0000_0000_0000_0000_0000, 16),
        ("fc00::/7", 0xfc00_0000_0000_0000_0000_0000_0000_0000, 7),
        ("fec0::/10", 0xfec0_0000_0000_0000_0000_0000_0000_0000, 10),
        ("fe80::/10", 0xfe80_0000_0000_0000_0000_0000_0000_0000, 10),
        ("ff00::/8", 0xff00_0000_0000_0000_0000_0000_0000_0000, 8),
    ];
    let value = u128::from(ip);
    NETWORKS.iter().find_map(|(label, network, prefix)| {
        in_cidr_v6(value, *network, *prefix)
            .then(|| format!("{ip} is in the blocked range {label}"))
    })
}

fn in_cidr_v4(value: u32, network: u32, prefix: u8) -> bool {
    let mask = u32::MAX << (32 - u32::from(prefix));
    value & mask == network & mask
}

fn in_cidr_v6(value: u128, network: u128, prefix: u8) -> bool {
    let mask = u128::MAX << (128 - u32::from(prefix));
    value & mask == network & mask
}

fn remaining_timeout(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| String::from("Safe fetch timed out"))
}

fn validate_http_url(url: &Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "Only http:// and https:// are supported: {}",
            redact_url(url)
        ));
    }
    if url.host_str().is_none() {
        return Err(String::from("Invalid URL: missing hostname"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(String::from("URL credentials are not allowed"));
    }
    Ok(())
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn declared_length(headers: &HeaderMap) -> Option<usize> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
}

fn redact_url(url: &Url) -> String {
    format!("{}{}", url.host_str().unwrap_or("<redacted>"), url.path())
}
