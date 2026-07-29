//! Admission policy for native page fetches.
//!
//! The search adapters talk to one fixed vendor authority, so their transport
//! can pin an exact domain. Native extraction fetches model-chosen URLs, which
//! makes admission the security boundary: every URL — the initial one and each
//! redirect hop — must pass these checks before any connection is opened, and
//! every address a host resolves to must clear the denied-network list before
//! it may be dialed. The policy is pure and synchronous so callers can re-run
//! it on every hop and every freshly resolved address without a transport in
//! hand.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::LazyLock;

use openwave_egress::CidrBlock;
use thiserror::Error;
use url::{Host, Url};

/// Longest URL accepted for a native page fetch, in bytes.
pub const MAX_FETCH_URL_BYTES: usize = crate::MAX_RESULT_URL_BYTES;

/// Why a URL or resolved address may not be fetched.
///
/// The enum is closed and carries no attacker-controlled text and, in
/// particular, no address. Which address a host resolved to *is* the sensitive
/// datum: it is an internal DNS answer for a name that a model — or a page
/// that redirected us — chose, so repeating it back would turn a refusal into
/// an internal-topology oracle. The address stays inside the check that made
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FetchPolicyViolation {
    #[error("page URL is not valid")]
    InvalidUrl,
    #[error("page URL exceeds the byte limit")]
    UrlTooLong,
    #[error("page URL scheme must be https")]
    SchemeNotHttps,
    #[error("page URL must not carry userinfo")]
    HasUserinfo,
    #[error("page URL has no host")]
    MissingHost,
    #[error("page URL must use the default https port")]
    ForbiddenPort,
    #[error("page address is not an allowed destination")]
    DeniedAddress,
}

/// Admit a model-supplied URL for fetching, or say precisely why not.
///
/// Admission requires `https`, an empty userinfo, a present host, and the
/// default port; the fragment is stripped from the returned URL because it is
/// never sent on the wire. An IP-literal host — in any encoding the URL parser
/// dials as an address, including decimal, hex, and octal forms — is vetted
/// against the denied-network list here. A DNS-named host is *not* resolved
/// here: the fetcher must pass every freshly resolved address through
/// [`admit_fetch_address`] before connecting, on every redirect hop.
pub fn admit_fetch_url(value: &str) -> Result<Url, FetchPolicyViolation> {
    if value.len() > MAX_FETCH_URL_BYTES {
        return Err(FetchPolicyViolation::UrlTooLong);
    }
    let mut parsed = Url::parse(value).map_err(|_| FetchPolicyViolation::InvalidUrl)?;
    if parsed.scheme() != "https" {
        return Err(FetchPolicyViolation::SchemeNotHttps);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(FetchPolicyViolation::HasUserinfo);
    }
    match parsed.host() {
        None => return Err(FetchPolicyViolation::MissingHost),
        Some(Host::Domain("")) => return Err(FetchPolicyViolation::MissingHost),
        Some(Host::Ipv4(address)) => admit_fetch_address(IpAddr::V4(address))?,
        Some(Host::Ipv6(address)) => admit_fetch_address(IpAddr::V6(address))?,
        Some(Host::Domain(_)) => {}
    }
    // The URL parser normalizes an explicit `:443` away for https, so any
    // remaining port is a non-default one.
    if parsed.port().is_some() {
        return Err(FetchPolicyViolation::ForbiddenPort);
    }
    parsed.set_fragment(None);
    Ok(parsed)
}

/// Admit one resolved (or literal) address, or refuse it as a destination.
///
/// The denied list covers loopback, unspecified, RFC 1918, link-local (which
/// includes cloud metadata services), CGNAT, benchmarking, documentation and
/// TEST-NET ranges, IPv6 unique-local and site-local, and
/// multicast/broadcast/reserved space.
///
/// An IPv6 address that embeds an IPv4 address is *also* judged by every IPv4
/// address it embeds, because the host stack will route it back onto the IPv4
/// internet: the mapped `::ffff:a.b.c.d` form, the NAT64 `64:ff9b::/96`
/// prefix, 6to4 `2002::/16`, and Teredo `2001::/32` (both the relay's address
/// and the obfuscated client address). So `2002:7f00:1::` is exactly as denied
/// as `127.0.0.1`.
pub fn admit_fetch_address(address: IpAddr) -> Result<(), FetchPolicyViolation> {
    let denied = match address {
        IpAddr::V4(v4) => is_denied_v4(v4),
        IpAddr::V6(v6) => {
            DENIED_V6_BLOCKS.iter().any(|block| block.contains(address))
                || embedded_v4_addresses(v6).into_iter().any(is_denied_v4)
        }
    };
    if denied {
        return Err(FetchPolicyViolation::DeniedAddress);
    }
    Ok(())
}

fn is_denied_v4(address: Ipv4Addr) -> bool {
    DENIED_V4_BLOCKS
        .iter()
        .any(|block| block.contains(IpAddr::V4(address)))
}

/// Every IPv4 address an IPv6 address embeds, across the transition formats a
/// host stack translates back to IPv4.
///
/// A format is only recognized by its prefix, so an address may yield no
/// embedded IPv4 at all; each one that is found is judged by the IPv4 list.
fn embedded_v4_addresses(address: Ipv6Addr) -> Vec<Ipv4Addr> {
    /// `64:ff9b::/96`, the well-known NAT64 prefix (RFC 6052). The local-use
    /// `64:ff9b:1::/48` prefix carries the IPv4 address at a position that
    /// varies with the prefix length, so it is denied wholesale instead.
    const NAT64_WELL_KNOWN: u128 = 0x0064_ff9b_0000_0000_0000_0000_0000_0000;

    let bits = u128::from(address);
    let embedded_at = |shift: u32| Ipv4Addr::from(((bits >> shift) & 0xffff_ffff) as u32);
    let mut addresses = Vec::new();
    if let Some(mapped) = address.to_ipv4_mapped() {
        addresses.push(mapped);
    }
    if bits >> 32 == NAT64_WELL_KNOWN >> 32 {
        addresses.push(embedded_at(0));
    }
    // 6to4 (RFC 3056): the 32 bits after the `2002::/16` prefix are the IPv4
    // tunnel endpoint, so `2002:7f00:1::` reaches 127.0.0.1.
    if bits >> 112 == 0x2002 {
        addresses.push(embedded_at(80));
    }
    // Teredo (RFC 4380): the 32 bits after the `2001::/32` prefix are the
    // server, and the last 32 bits are the client's IPv4 address stored as its
    // bitwise complement. Traffic can reach either, so both are judged.
    if bits >> 96 == 0x2001_0000 {
        addresses.push(embedded_at(64));
        addresses.push(Ipv4Addr::from(!((bits & 0xffff_ffff) as u32)));
    }
    addresses
}

static DENIED_V4_BLOCKS: LazyLock<[CidrBlock; 15]> = LazyLock::new(|| {
    [
        "0.0.0.0/8",       // unspecified and "this network"
        "10.0.0.0/8",      // RFC 1918
        "100.64.0.0/10",   // CGNAT shared address space
        "127.0.0.0/8",     // loopback
        "169.254.0.0/16",  // link-local, including cloud metadata endpoints
        "172.16.0.0/12",   // RFC 1918
        "192.0.0.0/24",    // IETF protocol assignments
        "192.0.2.0/24",    // TEST-NET-1
        "192.88.99.0/24",  // 6to4 relay anycast
        "192.168.0.0/16",  // RFC 1918
        "198.18.0.0/15",   // benchmarking, routed by some SD-WAN and VPN clients
        "198.51.100.0/24", // TEST-NET-2
        "203.0.113.0/24",  // TEST-NET-3
        "224.0.0.0/4",     // multicast
        "240.0.0.0/4",     // reserved, including the broadcast address
    ]
    .map(|block| CidrBlock::parse(block).expect("static deny CIDR must parse"))
});

static DENIED_V6_BLOCKS: LazyLock<[CidrBlock; 7]> = LazyLock::new(|| {
    [
        "::/96",          // unspecified, loopback, and deprecated IPv4-compatible space
        "64:ff9b:1::/48", // local-use NAT64 (RFC 8215)
        "2001:db8::/32",  // documentation
        "fc00::/7",       // unique local addresses
        "fe80::/10",      // link-local
        "fec0::/10",      // deprecated site-local, still configured on some networks
        "ff00::/8",       // multicast
    ]
    .map(|block| CidrBlock::parse(block).expect("static deny CIDR must parse"))
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_rejects_unsafe_urls_and_denied_hosts_in_every_encoding() {
        let cases: &[(&str, FetchPolicyViolation)] = &[
            ("http://example.com/", FetchPolicyViolation::SchemeNotHttps),
            ("ftp://example.com/", FetchPolicyViolation::SchemeNotHttps),
            (
                "https://user:pw@example.com/",
                FetchPolicyViolation::HasUserinfo,
            ),
            (
                "https://user@example.com/",
                FetchPolicyViolation::HasUserinfo,
            ),
            (
                "https://example.com:8443/",
                FetchPolicyViolation::ForbiddenPort,
            ),
            (
                "https://example.com:80/",
                FetchPolicyViolation::ForbiddenPort,
            ),
            ("data:text/html,hello", FetchPolicyViolation::SchemeNotHttps),
        ];
        for (url, expected) in cases {
            assert_eq!(admit_fetch_url(url).unwrap_err(), *expected, "{url}");
        }
        assert_eq!(
            admit_fetch_url(&format!(
                "https://example.com/{}",
                "a".repeat(MAX_FETCH_URL_BYTES)
            ))
            .unwrap_err(),
            FetchPolicyViolation::UrlTooLong
        );

        // IP-literal hosts in the ranges the fetcher must never dial, in the
        // alternate numeric encodings the URL parser resolves to addresses.
        for url in [
            "https://127.0.0.1/",
            "https://0x7f000001/", // hex encoding of 127.0.0.1
            "https://2130706433/", // decimal encoding of 127.0.0.1
            "https://0177.0.0.1/", // octal encoding of 127.0.0.1
            "https://127.1/",      // dotted-short encoding of 127.0.0.1
            "https://0.0.0.0/",
            "https://10.0.0.1/",
            "https://172.16.5.5/",
            "https://192.168.0.10/",
            "https://169.254.169.254/latest/meta-data/", // IMDS
            "https://100.64.0.1/",
            "https://224.0.0.1/",
            "https://255.255.255.255/",
            "https://[::1]/",
            "https://[::]/",
            "https://[fe80::1]/",
            "https://[fd00::1]/",
            "https://[ff02::1]/",
            "https://[::ffff:10.0.0.1]/", // IPv4-mapped RFC 1918
            "https://[64:ff9b::a00:1]/",  // NAT64-embedded 10.0.0.1
            "https://[2002:7f00:1::]/",   // 6to4-embedded 127.0.0.1
            "https://[2001:0:7f00:1::]/", // Teredo server 127.0.0.1
            "https://198.18.0.1/",
            "https://192.0.2.1/",
            "https://[fec0::1]/",
            "https://[2001:db8::1]/",
        ] {
            assert!(
                matches!(
                    admit_fetch_url(url),
                    Err(FetchPolicyViolation::DeniedAddress)
                ),
                "{url} was admitted"
            );
        }

        // Resolved addresses go through the same denial, mapped forms judged
        // by the embedded IPv4 address.
        for address in [
            "127.0.0.1",
            "10.255.255.255",
            "169.254.169.254",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:192.168.1.1",
            "fc00::1234",
            "64:ff9b::7f00:1",
            "64:ff9b:1::1",
            // Teredo relayed through a public server, but tunnelled to a
            // loopback client: the client address is the complement of
            // 127.0.0.1.
            "2001:0:5db8:d822::80ff:fffe",
            "192.88.99.1",
            "203.0.113.9",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(admit_fetch_address(address).is_err(), "{address} admitted");
        }
    }

    #[test]
    fn admission_accepts_public_destinations_and_strips_fragments() {
        let admitted = admit_fetch_url("https://example.com/a%20page?q=1#section").unwrap();
        assert_eq!(admitted.as_str(), "https://example.com/a%20page?q=1");
        // An explicit default port normalizes away rather than rejecting.
        assert!(admit_fetch_url("https://example.com:443/x").is_ok());
        assert!(admit_fetch_url("https://93.184.216.34/").is_ok());

        for address in [
            "93.184.216.34",
            "2606:2800:220:1:248:1893:25c8:1946",
            // Public IPv4 carried in mapped, NAT64, and 6to4 forms stays
            // admissible.
            "::ffff:93.184.216.34",
            "64:ff9b::5db8:d822",
            "2002:5db8:d822::1",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(admit_fetch_address(address).is_ok(), "{address} denied");
        }
    }
}
