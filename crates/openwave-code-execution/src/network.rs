//! Local loopback egress broker for native code execution.
//!
//! The broker is deliberately CONNECT-only. TLS stays end to end between the
//! package manager and registry; the broker decides only the requested host,
//! resolves it outside the sandbox, rejects private address space, and relays
//! bytes. A Seatbelt profile exposes exactly this listener port.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use openwave_core::NetworkPolicy;
use openwave_egress::{DomainPattern, EgressDestination};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{lookup_host, TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::{CodeExecutionError, PACKAGE_MANAGER_DOMAINS};

const MAX_CONNECT_HEADERS: usize = 16 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// One execution-scoped broker. Dropping it closes the listener and every
/// tunnel, so network authority cannot outlive the command that received it.
pub(crate) struct LocalEgressBroker {
    address: SocketAddr,
    task: JoinHandle<()>,
}

impl LocalEgressBroker {
    pub(crate) async fn start(policy: NetworkPolicy) -> Result<Self, CodeExecutionError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| {
                CodeExecutionError::Sandbox("could not bind the local egress broker".into())
            })?;
        let address = listener.local_addr().map_err(|_| {
            CodeExecutionError::Sandbox("could not inspect the local egress broker".into())
        })?;
        let task = tokio::spawn(serve(listener, policy));
        Ok(Self { address, task })
    }

    pub(crate) fn port(&self) -> u16 {
        self.address.port()
    }

    pub(crate) fn proxy_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.address.port())
    }
}

impl Drop for LocalEgressBroker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(listener: TcpListener, policy: NetworkPolicy) {
    serve_with_permits(
        listener,
        policy,
        Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS)),
    )
    .await;
}

async fn serve_with_permits(listener: TcpListener, policy: NetworkPolicy, permits: Arc<Semaphore>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    break;
                };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    tracing::debug!(%peer, "local egress broker connection cap reached");
                    drop(stream);
                    continue;
                };
                let policy = policy.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    if let Err(error) = handle_connection(stream, &policy).await {
                        tracing::debug!(%peer, %error, "local egress broker connection ended");
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn handle_connection(mut client: TcpStream, policy: &NetworkPolicy) -> Result<(), io::Error> {
    let request = match read_connect_request_with_timeout(&mut client, CONNECT_HEADER_TIMEOUT).await
    {
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
    relay_bidirectional(&mut client, &mut upstream, TUNNEL_IDLE_TIMEOUT).await
}

#[derive(Debug)]
struct ConnectRequest {
    authority: String,
}

async fn read_connect_request_with_timeout<S>(
    stream: &mut S,
    timeout: Duration,
) -> Result<ConnectRequest, &'static str>
where
    S: AsyncRead + Unpin,
{
    match tokio::time::timeout(timeout, read_connect_request(stream)).await {
        Ok(result) => result,
        Err(_) => Err("CONNECT headers timed out"),
    }
}

async fn read_connect_request<S>(stream: &mut S) -> Result<ConnectRequest, &'static str>
where
    S: AsyncRead + Unpin,
{
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
        if bytes.len() > MAX_CONNECT_HEADERS {
            return Err("CONNECT headers exceed the size limit");
        }
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

async fn relay_bidirectional<C, U>(
    client: &mut C,
    upstream: &mut U,
    idle_timeout: Duration,
) -> io::Result<()>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let mut client_open = true;
    let mut upstream_open = true;
    let mut client_buffer = [0_u8; 16 * 1024];
    let mut upstream_buffer = [0_u8; 16 * 1024];
    let idle = tokio::time::sleep(idle_timeout);
    tokio::pin!(idle);

    while client_open || upstream_open {
        tokio::select! {
            () = &mut idle => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "egress tunnel was idle for too long",
                ));
            }
            read = client.read(&mut client_buffer), if client_open => {
                let read = read?;
                if read == 0 {
                    client_open = false;
                    shutdown_with_timeout(upstream, idle_timeout).await?;
                    continue;
                }
                idle.as_mut().reset(Instant::now() + idle_timeout);
                write_all_with_timeout(upstream, &client_buffer[..read], idle_timeout).await?;
                idle.as_mut().reset(Instant::now() + idle_timeout);
            }
            read = upstream.read(&mut upstream_buffer), if upstream_open => {
                let read = read?;
                if read == 0 {
                    upstream_open = false;
                    shutdown_with_timeout(client, idle_timeout).await?;
                    continue;
                }
                idle.as_mut().reset(Instant::now() + idle_timeout);
                write_all_with_timeout(client, &upstream_buffer[..read], idle_timeout).await?;
                idle.as_mut().reset(Instant::now() + idle_timeout);
            }
        }
    }
    Ok(())
}

async fn write_all_with_timeout<W>(
    writer: &mut W,
    bytes: &[u8],
    timeout: Duration,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, writer.write_all(bytes))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "egress tunnel write timed out"))?
}

async fn shutdown_with_timeout<W>(writer: &mut W, timeout: Duration) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, writer.shutdown())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "egress tunnel shutdown timed out"))?
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

fn policy_permits(policy: &NetworkPolicy, host: &str, port: u16) -> bool {
    if EgressDestination::parse(host).is_err() {
        return false;
    }
    match policy {
        NetworkPolicy::Off => false,
        NetworkPolicy::PackageManagers => port == 443 && PACKAGE_MANAGER_DOMAINS.contains(&host),
        NetworkPolicy::AllowedHosts {
            allowed_hosts,
            package_managers,
        } => {
            allowed_hosts.iter().any(|allowed| {
                !allowed.starts_with("*.")
                    && DomainPattern::parse(allowed).is_ok_and(|pattern| pattern.matches(host))
            }) || (*package_managers && port == 443 && PACKAGE_MANAGER_DOMAINS.contains(&host))
        }
        NetworkPolicy::Open => true,
    }
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
    tracing::info!(
        target_host = host.unwrap_or("<invalid>"),
        target_port = port.unwrap_or_default(),
        decision = if allowed { "allow" } else { "deny" },
        reason,
        "local egress broker decision"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_policy_is_exact_and_https_only() {
        assert!(policy_permits(
            &NetworkPolicy::PackageManagers,
            "pypi.org",
            443
        ));
        assert!(!policy_permits(
            &NetworkPolicy::PackageManagers,
            "pypi.org",
            80
        ));
        assert!(!policy_permits(
            &NetworkPolicy::PackageManagers,
            "upload.pypi.org",
            443
        ));
        assert!(!policy_permits(
            &NetworkPolicy::PackageManagers,
            "evil-pypi.org",
            443
        ));
    }

    #[test]
    fn custom_policy_never_accepts_wildcards_or_suffix_tricks() {
        let policy = NetworkPolicy::AllowedHosts {
            allowed_hosts: vec!["api.example.com".into(), "*.unsafe.example".into()],
            package_managers: false,
        };
        assert!(policy_permits(&policy, "api.example.com", 8443));
        assert!(!policy_permits(&policy, "sub.api.example.com", 8443));
        assert!(!policy_permits(&policy, "x.unsafe.example", 8443));
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
            let address = address.parse().unwrap();
            assert!(is_restricted(address), "{address} was not restricted");
        }
        assert!(!is_restricted("8.8.8.8".parse().unwrap()));
        assert!(!is_restricted("2606:4700:4700::1111".parse().unwrap()));
    }

    #[tokio::test]
    async fn broker_rejects_loopback_immediately_even_under_open_policy() {
        let broker = LocalEgressBroker::start(NetworkPolicy::Open).await.unwrap();
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, broker.port()))
            .await
            .unwrap();
        stream
            .write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8(response)
            .unwrap()
            .starts_with("HTTP/1.1 403"));
    }

    #[tokio::test]
    async fn incomplete_connect_headers_time_out() {
        let (mut client, mut broker_side) = tokio::io::duplex(256);
        client
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n")
            .await
            .unwrap();

        let reason = read_connect_request_with_timeout(&mut broker_side, Duration::from_millis(10))
            .await
            .unwrap_err();

        assert_eq!(reason, "CONNECT headers timed out");
    }

    #[tokio::test]
    async fn oversized_connect_headers_are_rejected() {
        let (mut client, mut broker_side) = tokio::io::duplex(MAX_CONNECT_HEADERS + 1024);
        client
            .write_all(&vec![b'a'; MAX_CONNECT_HEADERS + 1])
            .await
            .unwrap();

        let reason = read_connect_request(&mut broker_side).await.unwrap_err();

        assert_eq!(reason, "CONNECT headers exceed the size limit");
    }

    #[tokio::test]
    async fn broker_closes_connections_above_its_concurrency_cap() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let permits = Arc::new(Semaphore::new(1));
        let task = tokio::spawn(serve_with_permits(
            listener,
            NetworkPolicy::Open,
            Arc::clone(&permits),
        ));

        let mut occupying = TcpStream::connect(address).await.unwrap();
        occupying
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while permits.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let mut excess = TcpStream::connect(address).await.unwrap();
        let mut byte = [0_u8; 1];
        let read = tokio::time::timeout(Duration::from_secs(1), excess.read(&mut byte))
            .await
            .unwrap()
            .unwrap();

        assert_eq!(read, 0);
        task.abort();
    }

    #[tokio::test]
    async fn an_idle_tunnel_is_closed() {
        let (mut client_side, _client_peer) = tokio::io::duplex(256);
        let (mut upstream_side, _upstream_peer) = tokio::io::duplex(256);

        let error = relay_bidirectional(
            &mut client_side,
            &mut upstream_side,
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "egress tunnel was idle for too long");
    }
}
