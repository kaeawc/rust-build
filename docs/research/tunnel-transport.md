# Tunnel transport: HTTP/2 multiplexing now, QUIC later

Research date: 2026-11-24 (issue #9). This covers what `rb-tunnel` ships (HTTP/2 multiplexing,
keepalives, idle timeouts) and the optional item: QUIC (quinn) as a transport.

## What shipped and why

| Need | Choice | Alternatives considered |
| --- | --- | --- |
| Many TCP connections over one TLS connection | HTTP/2 extended CONNECT ([RFC 8441](https://datatracker.ietf.org/doc/html/rfc8441)), `:protocol rb-tunnel`, one stream per TCP connection, raw bytes in DATA frames (the same shape as WebSockets over HTTP/2) | **connect-tcp** ([draft-ietf-httpbis-connect-tcp-18](https://www.ietf.org/archive/id/draft-ietf-httpbis-connect-tcp-18.html)): the standards-track successor, but it wraps bytes in DATA/FINAL_DATA capsules and is still a draft. **Classic CONNECT** ([RFC 9113 §8.5](https://datatracker.ietf.org/doc/html/rfc9113#section-8.5)): standard and interoperable, so the server accepts it too, but it has no `:protocol` to version our use. **yamux / custom framing**: new dependencies and a home-grown protocol. |
| Implementation | hyper 1 as-is: `http2::Builder::enable_connect_protocol()`, `hyper::ext::Protocol`, `hyper::upgrade::on`; the client checks `Connection::is_extended_connect_protocol_enabled()` after one probe round trip (server SETTINGS precede any response) | `h2` directly: more code for the same thing. |
| Fallback | ALPN decides per connection: `h2` = multiplexed, none = raw 1:1. A client whose server doesn't pick `h2`, or doesn't enable extended CONNECT, uses raw connections. | A version handshake of our own. |
| SNI routing | `tokio_rustls::LazyConfigAcceptor`: read the ClientHello, pick the `ServerConfig` and upstream by `server_name()` ([tokio-rustls docs](https://docs.rs/tokio-rustls)). rustls is 0.23.46, past the fragmented-ClientHello panic fixed in 0.23.18 ([RUSTSEC-2024-0399](https://rustsec.org/advisories/RUSTSEC-2024-0399.html)). | A `ResolvesServerCert` resolver: it can only pick the certificate, not the upstream. |
| Dead peers | HTTP/2 PING keepalive (`--keepalive`, hyper's `keep_alive_interval` and `keep_alive_timeout`) on multiplexed connections | TCP keepalive: coarse, and OS-dependent. |
| Abandoned streams | `--idle-timeout`: a stream or raw connection that moves no bytes either way for that long is closed. The rule runs on `rb_core::clock::Clock` (`IdleTracker`), so tests use the fake clock. | — |

No new crates: hyper, hyper-util, http, http-body-util and bytes were already in the graph.

## QUIC with quinn

**State (crates.io, 2026-11-24):** quinn 0.11.12 (updated 2026-09-14), widely used (about 166M
recent downloads). The default features are `log`, `platform-verifier`, `runtime-tokio`,
`rustls-ring` and `bloom`. The aws-lc backends are opt-in (`rustls-aws-lc-rs`), so the
`aws-lc-rs` ban holds with `default-features = false, features = ["runtime-tokio", "rustls-ring"]`.
The new crates would be `quinn-proto`, `quinn-udp`, `socket2`, `rustc-hash`, `web-time` and
`pin-project-lite`. Some of these may already be in the graph; `cargo deny` would decide.

**What it would buy:**

- No head-of-line blocking between streams. With HTTP/2 over one TCP connection, one lost packet
  stalls every tunnelled connection. QUIC streams are independent. This is the main gain on lossy
  or long-haul links.
- Connection migration when the client's IP changes (laptops, mobile), and 0-RTT/1-RTT setup.
- One QUIC stream per TCP connection maps directly to what the h2 mode does now: open a
  bidirectional stream, write a small target header, then relay.

**What it would cost:**

- A UDP listener next to the TCP one, and firewall rules that allow UDP. Many corporate networks
  block or throttle UDP, so TCP stays as the fallback (QUIC is an addition, not a replacement).
- A second framing for the target. The standards option is HTTP/3 + connect-tcp (or CONNECT-UDP
  ideas), which means `h3` + `h3-quinn` on top. Both are still pre-1.0.
- rustls QUIC configs differ from TLS ones (`quinn::crypto::rustls::QuicServerConfig`), so SNI
  routing and mTLS would have to be rebuilt for that path.
- Tuning: congestion control, UDP buffer sizes (quinn-udp uses GSO/GRO where it can).

**Recommendation:** don't add QUIC yet. HTTP/2 multiplexing covers the connection-count and
handshake-cost problems that motivated #9. Revisit when (a) a user needs the tunnel over lossy
links where HOL blocking shows up in measurements, or (b) connect-tcp and `h3` stabilise, so the
QUIC mode can follow a standard instead of a private framing. The shape would then be: a `quic`
cargo feature on `rb-tunnel` (off by default, like `otel`), `rb tunnel server --quic-listen`, and
a client `--mode quic` that falls back to `h2`, then `raw`.
