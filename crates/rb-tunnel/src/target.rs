//! How a multiplexed stream names its target.
//!
//! The client sends an HTTP/2 extended CONNECT (RFC 8441) with `:protocol` = [`PROTOCOL`] and
//! `:path` = `/tcp/{host}/{port}`, with the host percent-encoded (so `::1` is `%3A%3A1`). Once the
//! server answers 200, the stream's DATA frames carry the raw TCP bytes, as with WebSockets over
//! HTTP/2. The server also accepts a classic HTTP/2 CONNECT (RFC 9113 §8.5) whose `:authority` is
//! the target, so standard tooling can use it as a TCP proxy.
//!
//! The URI shape follows the template idea of the IETF connect-tcp draft
//! (draft-ietf-httpbis-connect-tcp), but this isn't that protocol: connect-tcp frames bytes in
//! capsules and is still a draft. A private token keeps the two from being confused.

/// The `:protocol` pseudo-header value for an rb tunnel stream.
pub const PROTOCOL: &str = "rb-tunnel";

const PREFIX: &str = "/tcp/";

/// `/tcp/{host}/{port}` for `host` (brackets around IPv6 are dropped) and `port`.
pub fn path(host: &str, port: u16) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let mut out = String::with_capacity(PREFIX.len() + host.len() + 8);
    out.push_str(PREFIX);
    for b in host.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out.push('/');
    out.push_str(&port.to_string());
    out
}

/// Inverse of [`path`]: the target `(host, port)`, or `None` if the path isn't one.
pub fn parse_path(path: &str) -> Option<(String, u16)> {
    let rest = path.strip_prefix(PREFIX)?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = rest.split_once('/')?;
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    let host = percent_decode(host)?;
    if host.is_empty() {
        return None;
    }
    Some((host, port))
}

/// `host:port` (or `[v6]:port`) into its parts, for `--target` and CONNECT authorities.
pub fn split_target(target: &str) -> Option<(String, u16)> {
    let (host, port) = crate::allow::split_host_port(target)?;
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_string(), port))
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'/' => return None,
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_names_and_ip_literals() {
        assert_eq!(path("db.internal", 5432), "/tcp/db.internal/5432");
        assert_eq!(path("[::1]", 80), "/tcp/%3A%3A1/80");
        assert_eq!(path("2001:db8::1", 443), "/tcp/2001%3Adb8%3A%3A1/443");
        for (h, p) in [("db.internal", 5432), ("::1", 80), ("10.0.0.1", 1), ("a_b~c", 65535)] {
            assert_eq!(parse_path(&path(h, p)), Some((h.to_string(), p)));
        }
        assert_eq!(parse_path("/tcp/host/22/"), Some(("host".into(), 22)));
    }

    #[test]
    fn rejects_paths_that_are_not_targets() {
        for bad in
            ["/", "/tcp/", "/tcp/host", "/tcp/host/0", "/tcp/host/x", "/tcp//80", "/tcp/a%2/80", "/tcp/a%FF/80", "/udp/h/1", "/tcp/a/b/80"]
        {
            assert_eq!(parse_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn splits_targets() {
        assert_eq!(split_target("db:5432"), Some(("db".into(), 5432)));
        assert_eq!(split_target("[::1]:80"), Some(("::1".into(), 80)));
        assert_eq!(split_target("db"), None);
        assert_eq!(split_target("db:0"), None);
        assert_eq!(split_target(":80"), None);
    }
}
