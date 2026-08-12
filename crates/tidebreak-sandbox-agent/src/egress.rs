//! The `egress-proxy` mode: the dual-homed side of the local Docker sandbox's
//! network boundary.
//!
//! The sandbox container's only network is an internal bridge with no route
//! out; this proxy is the one container attached to both that bridge and a
//! network with external reach. It serves two listeners:
//!
//! - **CONNECT egress** ([`DEFAULT_EGRESS_LISTEN`]): a CONNECT-only HTTP proxy
//!   with the same contract as the native local adapter's loopback broker
//!   (`tidebreak-code-execution`'s `network` module). TLS stays end to end; the
//!   proxy decides only the requested `host:port` against the compiled
//!   [`SandboxNetworkPolicy`], resolves the name *outside* the sandbox, refuses
//!   private/loopback/link-local answers (any private answer poisons the whole
//!   name, so resolver ordering cannot reopen a DNS-rebinding path), audits the
//!   decision, and relays bytes. Plain absolute-form HTTP is not implemented:
//!   plaintext egress is simply denied, as on the native path.
//! - **Transport relay** ([`DEFAULT_RELAY_LISTEN`]): a raw TCP relay to the
//!   sandbox container's supervisor port. A port published on a container whose
//!   only network is internal is not reachable from the host, so the host dials
//!   this proxy's published loopback port instead; the per-run transport secret
//!   still gates attach exactly as before.
//!
//! The policy is enforcement configuration, not advice: the sandbox's
//! `HTTP(S)_PROXY` environment merely points compliant tools here, while a
//! command that ignores it has no route anywhere. The policy arrives compiled
//! ([`POLICY_ENV`]) — destination classes already expanded to exact hosts by
//! the host — and an absent or malformed value means deny-all, never open.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tidebreak_egress::EgressDestination;
use tidebreak_sandbox_protocol::SandboxNetworkPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpListener, TcpStream};
use tokio::task::JoinSet;

/// Environment variable carrying the compiled [`SandboxNetworkPolicy`] as JSON.
/// Must match the name the local Docker backend injects.
pub const POLICY_ENV: &str = "TIDEBREAK_EGRESS_POLICY";
/// Environment variable overriding the CONNECT listener address.
pub const EGRESS_LISTEN_ENV: &str = "TIDEBREAK_EGRESS_LISTEN";
/// Environment variable overriding the transport-relay listener address.
pub const RELAY_LISTEN_ENV: &str = "TIDEBREAK_RELAY_LISTEN";
/// Environment variable naming the relay's upstream — the sandbox container's
/// supervisor endpoint on the internal network, as `host:port`. Absent means no
/// relay is served.
pub const RELAY_TARGET_ENV: &str = "TIDEBREAK_RELAY_TARGET";

/// Default CONNECT listener. The sandbox reaches it over the internal network;
/// it is never published to the host.
pub const DEFAULT_EGRESS_LISTEN: &str = "0.0.0.0:3128";
/// Default relay listener; the backend publishes this port to host loopback.
pub const DEFAULT_RELAY_LISTEN: &str = "0.0.0.0:8080";

const MAX_CONNECT_HEADERS: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How this proxy serves, resolved from the environment.
#[derive(Debug, Clone)]
pub struct EgressProxyConfig {
    /// The compiled policy every CONNECT is decided against.
    pub policy: SandboxNetworkPolicy,
    /// The CONNECT listener address.
    pub egress_listen: String,
    /// The transport-relay listener address.
    pub relay_listen: String,
    /// The relay's upstream `host:port`, or `None` to serve no relay.
    pub relay_target: Option<String>,
}

impl EgressProxyConfig {
    /// Resolve the configuration from process environment. A missing or
    /// malformed policy is deny-all — the proxy must fail closed, never open —
    /// and the malformed case is logged so the misconfiguration is visible.
    #[must_use]
    pub fn from_env() -> Self {
        let policy = match std::env::var(POLICY_ENV) {
            Ok(raw) => match serde_json::from_str::<SandboxNetworkPolicy>(&raw) {
                Ok(policy) => policy,
                Err(_) => {
                    eprintln!(
                        "tidebreak-sandbox-agent egress-proxy: {POLICY_ENV} is malformed; \
                         denying all egress (fail closed)"
                    );
                    SandboxNetworkPolicy::deny_all()
                }
            },
            Err(_) => SandboxNetworkPolicy::deny_all(),
        };
        Self {
            policy,
            egress_listen: std::env::var(EGRESS_LISTEN_ENV)
                .unwrap_or_else(|_| DEFAULT_EGRESS_LISTEN.to_owned()),
            relay_listen: std::env::var(RELAY_LISTEN_ENV)
                .unwrap_or_else(|_| DEFAULT_RELAY_LISTEN.to_owned()),
            relay_target: std::env::var(RELAY_TARGET_ENV)
                .ok()
                .filter(|target| !target.is_empty()),
        }
    }
}

/// A bound egress proxy: both listeners are held, so a caller that observed
/// [`bind`](Self::bind) succeed knows the ports are its own before serving.
pub struct EgressProxy {
    egress: TcpListener,
    relay: Option<(TcpListener, String)>,
    policy: SandboxNetworkPolicy,
}

impl EgressProxy {
    /// Bind both listeners.
    pub async fn bind(config: EgressProxyConfig) -> io::Result<Self> {
        let egress = TcpListener::bind(&config.egress_listen).await?;
        let relay = match config.relay_target {
            Some(target) => Some((TcpListener::bind(&config.relay_listen).await?, target)),
            None => None,
        };
        Ok(Self {
            egress,
            relay,
            policy: config.policy,
        })
    }

    /// The bound CONNECT listener address.
    pub fn egress_addr(&self) -> io::Result<SocketAddr> {
        self.egress.local_addr()
    }

    /// The bound relay listener address, when a relay target is configured.
    pub fn relay_addr(&self) -> Option<io::Result<SocketAddr>> {
        self.relay
            .as_ref()
            .map(|(listener, _)| listener.local_addr())
    }

    /// Serve both listeners until the process ends.
    pub async fn serve(self) {
        let Self {
            egress,
            relay,
            policy,
        } = self;
        match relay {
            Some((listener, target)) => {
                tokio::join!(serve_egress(egress, policy), serve_relay(listener, target));
            }
            None => serve_egress(egress, policy).await,
        }
    }
}

async fn serve_egress(listener: TcpListener, policy: SandboxNetworkPolicy) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    break;
                };
                let policy = policy.clone();
                connections.spawn(async move {
                    if let Err(error) = handle_connect(stream, &policy).await {
                        eprintln!("egress-proxy: connection from {peer} ended: {error}");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn serve_relay(listener: TcpListener, target: String) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let target = target.clone();
                connections.spawn(async move {
                    let mut client = stream;
                    let upstream = tokio::time::timeout(
                        CONNECT_TIMEOUT,
                        TcpStream::connect(target.as_str()),
                    )
                    .await;
                    match upstream {
                        Ok(Ok(mut upstream)) => {
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                        _ => {
                            eprintln!("egress-proxy: relay upstream {target} is unreachable");
                        }
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn handle_connect(
    mut client: TcpStream,
    policy: &SandboxNetworkPolicy,
) -> Result<(), io::Error> {
    let request = match read_connect_request(&mut client).await {
        Ok(request) => request,
        Err(reason) => {
            audit(None, None, false, reason);
            reject(&mut client, 400, "Bad Request").await?;
            return Ok(());
        }
    };
    let (host, port) = match parse_authority(&request.authority) {
        Some(target) => target,
        None => {
            audit(None, None, false, "invalid CONNECT authority");
            reject(&mut client, 400, "Bad Request").await?;
            return Ok(());
        }
    };
    if !policy_permits(policy, &host, port) {
        audit(
            Some(&host),
            Some(port),
            false,
            "destination denied by policy",
        );
        reject(&mut client, 403, "Forbidden").await?;
        return Ok(());
    }

    let addresses = match resolve_public(&host, port).await {
        Ok(addresses) => addresses,
        Err(reason) => {
            audit(Some(&host), Some(port), false, reason);
            reject(&mut client, 403, "Forbidden").await?;
            return Ok(());
        }
    };
    let upstream = match connect_any(&addresses).await {
        Ok(stream) => stream,
        Err(_) => {
            audit(Some(&host), Some(port), false, "upstream connection failed");
            reject(&mut client, 502, "Bad Gateway").await?;
            return Ok(());
        }
    };
    audit(Some(&host), Some(port), true, "destination allowed");
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    let mut upstream = upstream;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

struct ConnectRequest {
    authority: String,
}

async fn read_connect_request(stream: &mut TcpStream) -> Result<ConnectRequest, &'static str> {
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        if bytes.len() >= MAX_CONNECT_HEADERS {
            return Err("CONNECT headers exceed the size limit");
        }
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|_| "could not read CONNECT request")?;
        if read == 0 {
            return Err("CONNECT request ended before its headers");
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8(bytes).map_err(|_| "CONNECT headers are not UTF-8")?;
    let line = text
        .split("\r\n")
        .next()
        .ok_or("CONNECT request line is absent")?;
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or("CONNECT method is absent")?;
    let authority = parts.next().ok_or("CONNECT authority is absent")?;
    let version = parts.next().ok_or("CONNECT version is absent")?;
    if parts.next().is_some() || method != "CONNECT" || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err("only HTTP CONNECT is accepted");
    }
    Ok(ConnectRequest {
        authority: authority.to_owned(),
    })
}

fn parse_authority(authority: &str) -> Option<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, port) = rest.split_once("]:")?;
        let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
        return Some((host.to_ascii_lowercase(), port));
    }
    let (host, port) = authority.rsplit_once(':')?;
    if host.is_empty() || host.contains(':') {
        return None;
    }
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    Some((host.trim_end_matches('.').to_ascii_lowercase(), port))
}

/// Whether the compiled policy admits `host:port`. The destination must also be
/// a well-formed public name or address encoding ([`EgressDestination`]), so an
/// alternate IP spelling cannot slip past the exact-host comparison.
fn policy_permits(policy: &SandboxNetworkPolicy, host: &str, port: u16) -> bool {
    if EgressDestination::parse(host).is_err() {
        return false;
    }
    policy.permits(host, port)
}

async fn resolve_public(host: &str, port: u16) -> Result<Vec<SocketAddr>, &'static str> {
    if let Ok(address) = host.parse::<IpAddr>() {
        if is_restricted(address) {
            return Err("private, loopback, or link-local target");
        }
        return Ok(vec![SocketAddr::new(address, port)]);
    }
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| "destination DNS lookup failed")?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err("destination DNS lookup returned no addresses");
    }
    // Refuse the whole name when any answer is private. Selecting just a public
    // answer would make the decision depend on resolver ordering and reopen a
    // DNS-rebinding path on the next connection.
    if addresses.iter().any(|address| is_restricted(address.ip())) {
        return Err("destination resolved to private, loopback, or link-local space");
    }
    Ok(addresses)
}

async fn connect_any(addresses: &[SocketAddr]) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no upstream address");
    for address in addresses {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last = error,
            Err(_) => last = io::Error::new(io::ErrorKind::TimedOut, "upstream connect timed out"),
        }
    }
    Err(last)
}

fn is_restricted(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || in_v4_block(address, [100, 64, 0, 0], 10)
                || in_v4_block(address, [198, 18, 0, 0], 15)
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || in_v6_prefix(address, 0xfc00, 7)
                || in_v6_prefix(address, 0xfe80, 10)
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_restricted(IpAddr::V4(mapped)))
        }
    }
}

fn in_v4_block(address: Ipv4Addr, network: [u8; 4], prefix: u32) -> bool {
    let mask = u32::MAX << (32 - prefix);
    u32::from(address) & mask == u32::from(Ipv4Addr::from(network)) & mask
}

fn in_v6_prefix(address: Ipv6Addr, network: u16, prefix: u32) -> bool {
    let mask = u16::MAX << (16 - prefix);
    address.segments()[0] & mask == network & mask
}

async fn reject(stream: &mut TcpStream, code: u16, reason: &str) -> io::Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .await
}

fn audit(host: Option<&str>, port: Option<u16>, allowed: bool, reason: &'static str) {
    eprintln!(
        "egress-proxy: {} {}:{} ({reason})",
        if allowed { "allow" } else { "deny" },
        host.unwrap_or("<invalid>"),
        port.unwrap_or_default(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(hosts: &[&str], https_only: &[&str]) -> SandboxNetworkPolicy {
        SandboxNetworkPolicy {
            allow_all_public: false,
            allowed_hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            https_only_hosts: https_only.iter().map(|host| (*host).to_owned()).collect(),
        }
    }

    #[test]
    fn compiled_policy_is_exact_and_port_scoped() {
        let policy = allowlist(&["api.example.com"], &["pypi.org"]);
        assert!(policy_permits(&policy, "api.example.com", 8443));
        assert!(policy_permits(&policy, "pypi.org", 443));
        // The https-only class never opens other ports, and exact matching
        // never widens to subdomains or lookalikes.
        assert!(!policy_permits(&policy, "pypi.org", 80));
        assert!(!policy_permits(&policy, "sub.api.example.com", 8443));
        assert!(!policy_permits(&policy, "evil-pypi.org", 443));
        // Deny-all denies, open passes hygiene but stays name-validated.
        assert!(!policy_permits(
            &SandboxNetworkPolicy::deny_all(),
            "example.com",
            443
        ));
        assert!(policy_permits(
            &SandboxNetworkPolicy::open(),
            "example.com",
            443
        ));
        assert!(!policy_permits(
            &SandboxNetworkPolicy::open(),
            "not a host",
            443
        ));
    }

    #[test]
    fn private_and_local_address_families_are_restricted() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:127.0.0.1",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(is_restricted(address), "{address} was not restricted");
        }
        assert!(!is_restricted("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn a_malformed_or_absent_policy_denies_everything() {
        let deny: SandboxNetworkPolicy = Default::default();
        assert!(deny.denies_everything());
        // The env parser's fallback is this same deny-all value; the fallback
        // itself is exercised through `from_env` only in-process, so pin the
        // property the fallback relies on.
        assert!(!policy_permits(&deny, "example.com", 443));
    }

    async fn connect_via(proxy: SocketAddr, authority: &str) -> String {
        let mut stream = TcpStream::connect(proxy).await.unwrap();
        stream
            .write_all(
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();
        let mut response = vec![0_u8; 128];
        let read = stream.read(&mut response).await.unwrap();
        String::from_utf8_lossy(&response[..read]).into_owned()
    }

    /// The whole deny path, end to end over real sockets: a policy-denied name
    /// is 403, and even an Open policy refuses loopback — the two refusals the
    /// container's confinement depends on.
    #[tokio::test]
    async fn proxy_denies_by_policy_and_refuses_private_space() {
        let proxy = EgressProxy::bind(EgressProxyConfig {
            policy: SandboxNetworkPolicy::deny_all(),
            egress_listen: "127.0.0.1:0".to_owned(),
            relay_listen: "127.0.0.1:0".to_owned(),
            relay_target: None,
        })
        .await
        .unwrap();
        let addr = proxy.egress_addr().unwrap();
        tokio::spawn(proxy.serve());
        assert!(connect_via(addr, "example.com:443")
            .await
            .starts_with("HTTP/1.1 403"));

        let open = EgressProxy::bind(EgressProxyConfig {
            policy: SandboxNetworkPolicy::open(),
            egress_listen: "127.0.0.1:0".to_owned(),
            relay_listen: "127.0.0.1:0".to_owned(),
            relay_target: None,
        })
        .await
        .unwrap();
        let addr = open.egress_addr().unwrap();
        tokio::spawn(open.serve());
        assert!(connect_via(addr, "127.0.0.1:9")
            .await
            .starts_with("HTTP/1.1 403"));
        assert!(connect_via(addr, "not-connect")
            .await
            .starts_with("HTTP/1.1 400"));
    }

    /// The relay forwards raw bytes to its configured upstream — the property
    /// the host's attach transport rides on.
    #[tokio::test]
    async fn relay_forwards_to_the_configured_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut buffer = [0_u8; 5];
            stream.read_exact(&mut buffer).await.unwrap();
            assert_eq!(&buffer, b"hello");
            stream.write_all(b"world").await.unwrap();
        });

        let proxy = EgressProxy::bind(EgressProxyConfig {
            policy: SandboxNetworkPolicy::deny_all(),
            egress_listen: "127.0.0.1:0".to_owned(),
            relay_listen: "127.0.0.1:0".to_owned(),
            relay_target: Some(upstream_addr.to_string()),
        })
        .await
        .unwrap();
        let relay_addr = proxy.relay_addr().unwrap().unwrap();
        tokio::spawn(proxy.serve());

        let mut client = TcpStream::connect(relay_addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut buffer = [0_u8; 5];
        client.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"world");
    }
}
