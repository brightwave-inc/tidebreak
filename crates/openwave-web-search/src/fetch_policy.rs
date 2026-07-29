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

use std::net::{IpAddr, Ipv4Addr};
use std::sync::LazyLock;

use openwave_egress::CidrBlock;
use thiserror::Error;
use url::{Host, Url};

/// Longest URL accepted for a native page fetch, in bytes.
pub const MAX_FETCH_URL_BYTES: usize = crate::MAX_RESULT_URL_BYTES;

/// Why a URL or resolved address may not be fetched.
///
/// The enum is closed and carries no attacker-controlled text beyond the
/// denied address itself, so a reason can surface in diagnostics without
/// echoing an arbitrary URL.
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
    #[error("page address {0} is in a denied network range")]
    DeniedAddress(IpAddr),
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

/// Admit one resolved (or literal) address, or name it as denied.
///
/// The denied list covers loopback, unspecified, RFC 1918, link-local (which
/// includes cloud metadata services), CGNAT, IPv6 unique-local, and
/// multicast/broadcast/reserved space. An IPv6 address that embeds an IPv4
/// address — the mapped `::ffff:a.b.c.d` form and the NAT64 `64:ff9b::/96`
/// prefix — is judged by the embedded IPv4 address, so `::ffff:10.0.0.1` is
/// exactly as denied as `10.0.0.1`.
pub fn admit_fetch_address(address: IpAddr) -> Result<(), FetchPolicyViolation> {
    let denied = match address {
        IpAddr::V4(v4) => is_denied_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                is_denied_v4(mapped)
            } else if let Some(embedded) = nat64_embedded_v4(u128::from(v6)) {
                is_denied_v4(embedded)
            } else {
                DENIED_V6_BLOCKS.iter().any(|block| block.contains(address))
            }
        }
    };
    if denied {
        return Err(FetchPolicyViolation::DeniedAddress(address));
    }
    Ok(())
}

fn is_denied_v4(address: Ipv4Addr) -> bool {
    DENIED_V4_BLOCKS
        .iter()
        .any(|block| block.contains(IpAddr::V4(address)))
}

/// The IPv4 address a NAT64 (`64:ff9b::/96`) IPv6 address translates to.
fn nat64_embedded_v4(bits: u128) -> Option<Ipv4Addr> {
    const NAT64_PREFIX: u128 = 0x0064_ff9b_0000_0000_0000_0000_0000_0000;
    (bits >> 32 == NAT64_PREFIX >> 32).then(|| Ipv4Addr::from((bits & 0xffff_ffff) as u32))
}

static DENIED_V4_BLOCKS: LazyLock<[CidrBlock; 9]> = LazyLock::new(|| {
    [
        "0.0.0.0/8",      // unspecified and "this network"
        "10.0.0.0/8",     // RFC 1918
        "100.64.0.0/10",  // CGNAT shared address space
        "127.0.0.0/8",    // loopback
        "169.254.0.0/16", // link-local, including cloud metadata endpoints
        "172.16.0.0/12",  // RFC 1918
        "192.168.0.0/16", // RFC 1918
        "224.0.0.0/4",    // multicast
        "240.0.0.0/4",    // reserved, including the broadcast address
    ]
    .map(|block| CidrBlock::parse(block).expect("static deny CIDR must parse"))
});

static DENIED_V6_BLOCKS: LazyLock<[CidrBlock; 4]> = LazyLock::new(|| {
    [
        "::/96",     // unspecified, loopback, and deprecated IPv4-compatible space
        "fc00::/7",  // unique local addresses
        "fe80::/10", // link-local
        "ff00::/8",  // multicast
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
        ] {
            assert!(
                matches!(
                    admit_fetch_url(url),
                    Err(FetchPolicyViolation::DeniedAddress(_))
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
            // Public IPv4 carried in mapped and NAT64 forms stays admissible.
            "::ffff:93.184.216.34",
            "64:ff9b::5db8:d822",
        ] {
            let address: IpAddr = address.parse().unwrap();
            assert!(admit_fetch_address(address).is_ok(), "{address} denied");
        }
    }
}
