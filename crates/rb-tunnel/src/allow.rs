//! Which `host:port` targets a tunnel server will dial for a multiplexed stream.
//!
//! Rules are `HOST:PORTS`, where `HOST` is an exact name or IP (`db.internal`, `10.0.0.5`,
//! `[::1]`) or a CIDR (`10.0.0.0/8`, `[fd00::/8]`), and `PORTS` is a port, a range `lo-hi`, or
//! `*`. Names match case-insensitively without resolving. CIDR rules are checked against the
//! *resolved* addresses, and only those addresses are dialled, so a hostname can't smuggle a
//! target past an IP rule.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
enum HostMatch {
    /// Lowercased name or IP literal (no brackets).
    Exact(String),
    Cidr {
        net: IpAddr,
        prefix: u8,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    host: HostMatch,
    ports: (u16, u16),
}

impl Rule {
    fn port_ok(&self, port: u16) -> bool {
        (self.ports.0..=self.ports.1).contains(&port)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParseRuleError(String);

impl fmt::Display for ParseRuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid allow rule: {}", self.0)
    }
}

impl std::error::Error for ParseRuleError {}

impl FromStr for Rule {
    type Err = ParseRuleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |why: &str| ParseRuleError(format!("{s:?}: {why} (want HOST:PORT, HOST:LO-HI, CIDR:PORTS or HOST:*)"));
        let (host, ports) = split_host_port(s).ok_or_else(|| err("missing :PORTS"))?;
        let ports = match ports {
            "*" => (1, u16::MAX),
            p => match p.split_once('-') {
                Some((lo, hi)) => {
                    let lo = lo.parse::<u16>().map_err(|_| err("bad port"))?;
                    let hi = hi.parse::<u16>().map_err(|_| err("bad port"))?;
                    if lo > hi {
                        return Err(err("port range is reversed"));
                    }
                    (lo, hi)
                }
                None => {
                    let p = p.parse::<u16>().map_err(|_| err("bad port"))?;
                    (p, p)
                }
            },
        };
        if host.is_empty() {
            return Err(err("empty host"));
        }
        let host = match host.split_once('/') {
            Some((net, prefix)) => {
                let net = net.parse::<IpAddr>().map_err(|_| err("bad CIDR address"))?;
                let max = if net.is_ipv4() { 32 } else { 128 };
                let prefix = prefix.parse::<u8>().ok().filter(|p| *p <= max).ok_or_else(|| err("bad CIDR prefix"))?;
                HostMatch::Cidr { net, prefix }
            }
            None => HostMatch::Exact(normalize(host)),
        };
        Ok(Rule { host, ports })
    }
}

/// Splits `host:port`, `[v6]:port` and `[v6/len]:port`. The port part may be `*` or a range.
pub(crate) fn split_host_port(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        return Some((host, rest.strip_prefix(':')?));
    }
    let (host, port) = s.rsplit_once(':')?;
    // A bare IPv6 address (`::1:80`) is ambiguous; require brackets.
    if host.contains(':') {
        return None;
    }
    Some((host, port))
}

/// Canonical form for comparing hosts: lowercase, no brackets, IPs in their standard notation.
fn normalize(host: &str) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => host.trim_end_matches('.').to_ascii_lowercase(),
    }
}

fn in_cidr(ip: IpAddr, net: IpAddr, prefix: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(ip) & mask == u32::from(net) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(ip) & mask == u128::from(net) & mask
        }
        // IPv4-mapped IPv6 (`::ffff:10.0.0.1`) is matched as IPv4.
        (IpAddr::V6(ip), IpAddr::V4(_)) => ip.to_ipv4_mapped().is_some_and(|v4| in_cidr(IpAddr::V4(v4), net, prefix)),
        (IpAddr::V4(_), IpAddr::V6(_)) => false,
    }
}

/// Why a target was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Denied {
    /// No rule allows it (403).
    NotAllowed,
    /// The name didn't resolve (502).
    Resolve(String),
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Denied::NotAllowed => f.write_str("target not allowed"),
            Denied::Resolve(e) => write!(f, "resolve: {e}"),
        }
    }
}

/// An ordered set of [`Rule`]s. Empty allows nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allowlist {
    rules: Vec<Rule>,
}

impl Allowlist {
    pub fn new(rules: Vec<Rule>) -> Self {
        Allowlist { rules }
    }

    /// Parses each rule; the first bad one is the error.
    pub fn parse<S: AsRef<str>>(rules: &[S]) -> Result<Self, ParseRuleError> {
        rules.iter().map(|r| r.as_ref().parse()).collect::<Result<_, _>>().map(Allowlist::new)
    }

    /// Adds an exact `host:port` (e.g. the fixed upstream, which is always reachable).
    pub fn with_exact(mut self, addr: &str) -> Result<Self, ParseRuleError> {
        self.rules.push(addr.parse()?);
        Ok(self)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// True if an exact-name rule allows `host:port` without looking at addresses.
    pub fn allows_name(&self, host: &str, port: u16) -> bool {
        let host = normalize(host);
        self.rules.iter().any(|r| r.port_ok(port) && matches!(&r.host, HostMatch::Exact(h) if *h == host))
    }

    /// True if an exact-IP or CIDR rule allows this address.
    pub fn allows_addr(&self, addr: SocketAddr) -> bool {
        let ip = addr.ip();
        self.rules.iter().any(|r| {
            r.port_ok(addr.port())
                && match &r.host {
                    HostMatch::Cidr { net, prefix } => in_cidr(ip, *net, *prefix),
                    HostMatch::Exact(h) => h.parse::<IpAddr>().is_ok_and(|rule_ip| rule_ip == ip),
                }
        })
    }

    /// Filters resolved addresses for `host:port`: all of them if the name itself is allowed,
    /// otherwise only those an IP/CIDR rule allows.
    pub fn filter(&self, host: &str, port: u16, resolved: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, Denied> {
        if self.allows_name(host, port) {
            return Ok(resolved);
        }
        let allowed: Vec<_> = resolved.into_iter().filter(|a| self.allows_addr(*a)).collect();
        if allowed.is_empty() { Err(Denied::NotAllowed) } else { Ok(allowed) }
    }

    /// Resolves `host:port` and returns the addresses that may be dialled.
    pub async fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, Denied> {
        // Cheap refusal before touching DNS: nothing could match.
        if self.rules.iter().all(|r| !r.port_ok(port)) {
            return Err(Denied::NotAllowed);
        }
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let resolved: Vec<SocketAddr> = match bare.parse::<IpAddr>() {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => {
                if !self.allows_name(bare, port) && !self.rules.iter().any(|r| matches!(r.host, HostMatch::Cidr { .. })) {
                    return Err(Denied::NotAllowed);
                }
                tokio::net::lookup_host((bare, port)).await.map_err(|e| Denied::Resolve(e.to_string()))?.collect()
            }
        };
        self.filter(bare, port, resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().expect("socket addr")
    }

    #[test]
    fn parses_every_rule_form() {
        let a =
            Allowlist::parse(&["db.internal:5432", "10.0.0.0/8:8000-8100", "[::1]:80", "[fd00::/8]:*", "Example.COM.:443"]).expect("parse");
        assert!(a.allows_name("DB.internal", 5432));
        assert!(!a.allows_name("db.internal", 5433));
        assert!(a.allows_name("example.com", 443));
        assert!(a.allows_addr(addr("10.1.2.3:8050")));
        assert!(!a.allows_addr(addr("10.1.2.3:8101")));
        assert!(!a.allows_addr(addr("11.0.0.1:8050")));
        assert!(a.allows_addr(addr("[::1]:80")));
        assert!(a.allows_addr(addr("[fd12::5]:1")));
        assert!(!a.allows_addr(addr("[fe80::1]:1")));
    }

    #[test]
    fn rejects_malformed_rules() {
        for bad in ["db.internal", ":80", "host:0-", "host:9-1", "10.0.0.0/33:1", "::1:80", "host:99999", "[::1:80"] {
            assert!(bad.parse::<Rule>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn cidr_edges_and_mapped_addresses() {
        assert!(in_cidr("0.0.0.1".parse().expect("ip"), "0.0.0.0".parse().expect("ip"), 0));
        assert!(in_cidr("192.168.1.255".parse().expect("ip"), "192.168.1.0".parse().expect("ip"), 24));
        assert!(!in_cidr("192.168.2.0".parse().expect("ip"), "192.168.1.0".parse().expect("ip"), 24));
        assert!(in_cidr("::ffff:10.9.9.9".parse().expect("ip"), "10.0.0.0".parse().expect("ip"), 8));
        assert!(in_cidr("10.0.0.7".parse().expect("ip"), "10.0.0.7".parse().expect("ip"), 32));
    }

    #[test]
    fn names_resolve_only_to_cidr_allowed_addresses() {
        let a = Allowlist::parse(&["127.0.0.0/8:1-65535"]).expect("parse");
        // A name that resolved to one inside and one outside address keeps only the inside one.
        let got = a.filter("svc.example", 80, vec![addr("127.0.0.2:80"), addr("93.184.216.34:80")]).expect("allowed");
        assert_eq!(got, vec![addr("127.0.0.2:80")]);
        assert_eq!(a.filter("svc.example", 80, vec![addr("93.184.216.34:80")]), Err(Denied::NotAllowed));
    }

    #[tokio::test]
    async fn resolve_checks_literals_without_dns_and_refuses_empty_lists() {
        let a = Allowlist::default().with_exact("127.0.0.1:9").expect("exact");
        assert_eq!(a.resolve("127.0.0.1", 9).await, Ok(vec![addr("127.0.0.1:9")]));
        assert_eq!(a.resolve("127.0.0.1", 10).await, Err(Denied::NotAllowed));
        assert_eq!(a.resolve("not-listed.invalid", 9).await, Err(Denied::NotAllowed));
        assert_eq!(Allowlist::default().resolve("127.0.0.1", 9).await, Err(Denied::NotAllowed));
        let l = Allowlist::parse(&["localhost:9"]).expect("parse");
        assert!(l.resolve("localhost", 9).await.is_ok_and(|v| !v.is_empty()));
    }
}
