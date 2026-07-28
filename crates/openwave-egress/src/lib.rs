//! The egress policy decision layer.
//!
//! One dependency-free module answers one question — may this workload open a
//! connection to this destination? — and every enforcement point consults it:
//! the local execution adapter (which denies network outright today), the
//! future sandbox supervisor, and provider-level controls where a managed
//! vendor exposes them. Keeping it std-only is deliberate: the in-sandbox
//! supervisor must be able to consult the same decision without pulling an
//! HTTP client or async runtime into the sandbox image.
//!
//! The policy is an allowlist and the answer is deny by default: a
//! destination is permitted only when a granted domain pattern or address
//! block matches it. [`EgressPolicy::BlockAll`] and an empty allowlist both
//! deny everything.
//!
//! Enforcement is tiered, and the tier is stated rather than implied
//! ([`EnforcementTier`]). Policy enforced from outside the sandbox is a
//! boundary; supervisor-enforced policy shares a failure domain with the
//! workload and is defense in depth. Whether a backend has the external tier
//! is host knowledge — a capability the host establishes itself or one
//! compiled into a shipped adapter ([`EgressEnforcement`]) — never a claim a
//! backend makes about itself. The declaration describes what the enforcement
//! actually blocks, vendor exceptions included, and the admission rule for
//! third-party-credential-bearing work runs against that honest declaration.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Longest accepted destination host or domain pattern, in bytes.
pub const MAX_DOMAIN_BYTES: usize = 253;

/// A rejected pattern, address block, or destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressError {
    InvalidDomainPattern(String),
    InvalidCidr(String),
    InvalidDestination(String),
}

impl fmt::Display for EgressError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDomainPattern(value) => {
                write!(formatter, "invalid egress domain pattern: {value}")
            }
            Self::InvalidCidr(value) => {
                write!(formatter, "invalid egress address block: {value}")
            }
            Self::InvalidDestination(value) => {
                write!(formatter, "invalid egress destination: {value}")
            }
        }
    }
}

impl std::error::Error for EgressError {}

/// An exact host (`api.example.com`) or a single leading wildcard
/// (`*.example.com`) over lowercase DNS labels.
///
/// A wildcard matches any subdomain but never the bare suffix itself:
/// `*.example.com` matches `files.example.com`, not `example.com`. Grants
/// stay as narrow as they read.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DomainPattern {
    /// Lowercased suffix without the wildcard marker.
    suffix: String,
    wildcard: bool,
}

impl DomainPattern {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, EgressError> {
        let value = value.as_ref();
        let invalid = || EgressError::InvalidDomainPattern(value.to_owned());
        let (wildcard, host) = match value.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let host = host.to_ascii_lowercase();
        validate_host(&host).ok_or_else(invalid)?;
        Ok(Self {
            suffix: host,
            wildcard,
        })
    }

    #[must_use]
    pub fn matches(&self, host: &str) -> bool {
        if host.len() > MAX_DOMAIN_BYTES {
            return false;
        }
        let host = host.to_ascii_lowercase();
        if self.wildcard {
            host.strip_suffix(self.suffix.as_str())
                .and_then(|prefix| prefix.strip_suffix('.'))
                .is_some_and(|labels| !labels.is_empty())
        } else {
            host == self.suffix
        }
    }
}

impl fmt::Display for DomainPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.wildcard {
            write!(formatter, "*.{}", self.suffix)
        } else {
            formatter.write_str(&self.suffix)
        }
    }
}

/// Accept lowercase DNS-shaped hosts: dot-separated labels of
/// `[a-z0-9-]`, no hyphen at a label edge, not an IP literal.
fn validate_host(host: &str) -> Option<()> {
    if host.is_empty() || host.len() > MAX_DOMAIN_BYTES || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let valid = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    valid.then_some(())
}

/// An IPv4 or IPv6 address block in CIDR notation; a bare address is the
/// single-address block. The stored address is masked to the network address,
/// so `10.0.0.5/8` and `10.0.0.0/8` are the same block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CidrBlock {
    network: IpAddr,
    prefix: u8,
}

impl CidrBlock {
    pub fn parse(value: impl AsRef<str>) -> Result<Self, EgressError> {
        let value = value.as_ref();
        let invalid = || EgressError::InvalidCidr(value.to_owned());
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) => {
                let address: IpAddr = address.parse().map_err(|_| invalid())?;
                // Reject leading zeros / signs that `u8: FromStr` would accept.
                if !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid());
                }
                (address, prefix.parse::<u8>().map_err(|_| invalid())?)
            }
            None => {
                let address: IpAddr = value.parse().map_err(|_| invalid())?;
                let prefix = match address {
                    IpAddr::V4(_) => 32,
                    IpAddr::V6(_) => 128,
                };
                (address, prefix)
            }
        };
        let bits = match address {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > bits {
            return Err(invalid());
        }
        Ok(Self {
            network: mask_address(address, prefix),
            prefix,
        })
    }

    #[must_use]
    pub fn contains(&self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                mask_address(address, self.prefix) == self.network
            }
            // A mixed-family comparison is a miss, never a mapping guess.
            _ => false,
        }
    }
}

impl fmt::Display for CidrBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.network, self.prefix)
    }
}

fn mask_address(address: IpAddr, prefix: u8) -> IpAddr {
    match address {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - u32::from(prefix))
            };
            IpAddr::V4(Ipv4Addr::from(bits & mask))
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - u32::from(prefix))
            };
            IpAddr::V6(Ipv6Addr::from(bits & mask))
        }
    }
}

/// One destination a workload wants to reach: a DNS name before resolution,
/// or a literal address.
///
/// The layer never resolves names. A domain destination is decided against
/// domain patterns and an address destination against address blocks; a
/// domain grant says nothing about the addresses it resolves to, and
/// vice versa.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EgressDestination {
    Domain(String),
    Address(IpAddr),
}

impl EgressDestination {
    /// Classify a connect target: an IP literal is an address, anything else
    /// must be a valid lowercase host name.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, EgressError> {
        let value = value.as_ref();
        if let Ok(address) = value.parse::<IpAddr>() {
            return Ok(Self::Address(address));
        }
        let host = value.to_ascii_lowercase();
        validate_host(&host).ok_or_else(|| EgressError::InvalidDestination(value.to_owned()))?;
        Ok(Self::Domain(host))
    }
}

/// The granted allowlist: domain patterns and address blocks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EgressAllowlist {
    domains: Vec<DomainPattern>,
    cidrs: Vec<CidrBlock>,
}

impl EgressAllowlist {
    #[must_use]
    pub fn new(domains: Vec<DomainPattern>, cidrs: Vec<CidrBlock>) -> Self {
        Self { domains, cidrs }
    }

    #[must_use]
    pub fn domains(&self) -> &[DomainPattern] {
        &self.domains
    }

    #[must_use]
    pub fn cidrs(&self) -> &[CidrBlock] {
        &self.cidrs
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty() && self.cidrs.is_empty()
    }
}

/// The one policy every enforcement point consults. Deny by default: only an
/// explicit grant permits a destination, and an empty allowlist is
/// indistinguishable from [`EgressPolicy::BlockAll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPolicy {
    BlockAll,
    Allowlist(EgressAllowlist),
}

impl EgressPolicy {
    /// May a workload under this policy open a connection to `destination`?
    #[must_use]
    pub fn permits(&self, destination: &EgressDestination) -> bool {
        let Self::Allowlist(allowlist) = self else {
            return false;
        };
        match destination {
            EgressDestination::Domain(host) => allowlist
                .domains
                .iter()
                .any(|pattern| pattern.matches(host)),
            EgressDestination::Address(address) => {
                allowlist.cidrs.iter().any(|block| block.contains(*address))
            }
        }
    }
}

/// Where an egress policy is enforced relative to the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnforcementTier {
    /// Enforced from outside the sandbox — the OS denying network to a local
    /// process, a host-configured firewall, a vendor's per-sandbox network
    /// policy. A boundary.
    External,
    /// Enforced by the in-sandbox supervisor, which shares a failure domain
    /// with the workload. Defense in depth, never a boundary.
    Supervisor,
}

/// How far one enforcement exception reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionReach {
    /// A fixed, narrow service that is not a general-purpose destination.
    Narrow,
    /// A general-purpose destination stays reachable — a ready exfiltration
    /// channel (public git hosting, registries that accept uploads).
    GeneralPurpose,
}

/// What one exception leaves reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExceptionScope {
    /// A destination matched by name.
    Domain(DomainPattern),
    /// A destination matched by address.
    Address(CidrBlock),
    /// Domain-pattern rules are enforced only on these ports; a destination
    /// denied only by a domain rule may stay reachable on any other port.
    DomainRulePortLimit(Vec<u16>),
    /// A vendor-curated destination set the host cannot enumerate (an
    /// "essential services" list the vendor maintains and may change).
    VendorCurated,
}

/// One hole a backend's enforcement leaves open regardless of the configured
/// policy, declared honestly instead of hidden behind a feature name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementException {
    pub scope: ExceptionScope,
    pub reach: ExceptionReach,
    /// The vendor's stated purpose for the hole ("DNS resolution",
    /// "package registries", …) for settings and audit surfaces.
    pub purpose: &'static str,
}

/// A backend's egress enforcement, declared as what it actually blocks.
///
/// This is host knowledge: the host establishes it itself (the boundary it
/// configures around a local container) or ships it compiled into a managed
/// vendor's adapter. It is never populated from anything a backend claims
/// about itself over the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressEnforcement {
    tier: EnforcementTier,
    exceptions: Vec<EnforcementException>,
}

impl EgressEnforcement {
    /// Enforcement applied from outside the sandbox, with every hole the
    /// vendor's mechanism leaves open stated up front.
    #[must_use]
    pub fn external(exceptions: Vec<EnforcementException>) -> Self {
        Self {
            tier: EnforcementTier::External,
            exceptions,
        }
    }

    /// Enforcement carried only by the in-sandbox supervisor.
    #[must_use]
    pub fn supervisor() -> Self {
        Self {
            tier: EnforcementTier::Supervisor,
            exceptions: Vec::new(),
        }
    }

    #[must_use]
    pub fn tier(&self) -> EnforcementTier {
        self.tier
    }

    #[must_use]
    pub fn exceptions(&self) -> &[EnforcementException] {
        &self.exceptions
    }

    /// The admission rule for third-party-credential-bearing work: only the
    /// external tier qualifies, and an enforcement surface whose exceptions
    /// leave a general-purpose destination reachable does not qualify no
    /// matter what the vendor calls the feature.
    #[must_use]
    pub fn is_credential_boundary(&self) -> bool {
        self.tier == EnforcementTier::External
            && self
                .exceptions
                .iter()
                .all(|exception| exception.reach == ExceptionReach::Narrow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(domains: &[&str], cidrs: &[&str]) -> EgressPolicy {
        EgressPolicy::Allowlist(EgressAllowlist::new(
            domains
                .iter()
                .map(|pattern| DomainPattern::parse(pattern).unwrap())
                .collect(),
            cidrs
                .iter()
                .map(|block| CidrBlock::parse(block).unwrap())
                .collect(),
        ))
    }

    fn domain(host: &str) -> EgressDestination {
        EgressDestination::parse(host).unwrap()
    }

    #[test]
    fn policy_denies_by_default_and_permits_only_explicit_grants() {
        let policy = allowlist(&["api.example.com", "*.pypi.org"], &["140.82.112.0/20"]);

        assert!(policy.permits(&domain("api.example.com")));
        assert!(policy.permits(&domain("Files.PYPI.org")));
        assert!(policy.permits(&domain("140.82.114.4")));

        // Wildcards never widen to the bare suffix, other subdomains, or
        // suffix tricks; addresses outside the block and unrelated hosts miss.
        assert!(!policy.permits(&domain("pypi.org")));
        assert!(!policy.permits(&domain("evil-pypi.org")));
        assert!(!policy.permits(&domain("sub.api.example.com")));
        assert!(!policy.permits(&domain("140.82.128.4")));
        assert!(!policy.permits(&domain("attacker.example")));

        // Domain grants decide names and CIDR grants decide addresses;
        // neither crosses over, and IPv6 never matches a v4 block.
        assert!(!allowlist(&["*.example.com"], &[]).permits(&domain("93.184.216.34")));
        assert!(!allowlist(&[], &["0.0.0.0/0"]).permits(&domain("example.com")));
        assert!(!allowlist(&[], &["0.0.0.0/0"]).permits(&domain("::1")));

        assert!(!EgressPolicy::BlockAll.permits(&domain("example.com")));
        assert!(!allowlist(&[], &[]).permits(&domain("example.com")));
    }

    #[test]
    fn malformed_grants_and_destinations_are_rejected_at_parse() {
        for pattern in [
            "",
            "*.",
            "*",
            "a.*.example.com",
            "exa mple.com",
            "-a.example.com",
            "8.8.8.8",
        ] {
            assert!(DomainPattern::parse(pattern).is_err(), "{pattern:?} parsed");
        }
        for cidr in [
            "",
            "10.0.0.0/33",
            "::/129",
            "example.com",
            "10.0.0.0/ 8",
            "10.0.0.0/+8",
        ] {
            assert!(CidrBlock::parse(cidr).is_err(), "{cidr:?} parsed");
        }
        assert!(EgressDestination::parse("not a host").is_err());

        // Non-network addresses are masked to their block, and bare
        // addresses are the single-address block.
        assert_eq!(
            CidrBlock::parse("10.0.0.5/8").unwrap(),
            CidrBlock::parse("10.0.0.0/8").unwrap()
        );
        assert_eq!(
            CidrBlock::parse("8.8.8.8").unwrap().to_string(),
            "8.8.8.8/32"
        );
        assert!(CidrBlock::parse("2001:db8::/32")
            .unwrap()
            .contains("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn credential_admission_runs_against_the_declared_exceptions() {
        // The local adapter's outright network denial: external, no holes.
        assert!(EgressEnforcement::external(Vec::new()).is_credential_boundary());

        // A narrow, fixed exception keeps the boundary.
        let narrow = EgressEnforcement::external(vec![EnforcementException {
            scope: ExceptionScope::Address(CidrBlock::parse("8.8.8.8/32").unwrap()),
            reach: ExceptionReach::Narrow,
            purpose: "DNS resolution",
        }]);
        assert!(narrow.is_credential_boundary());

        // One general-purpose hole disqualifies the whole surface, whatever
        // the vendor calls the feature.
        let leaky = EgressEnforcement::external(vec![EnforcementException {
            scope: ExceptionScope::VendorCurated,
            reach: ExceptionReach::GeneralPurpose,
            purpose: "git hosting",
        }]);
        assert!(!leaky.is_credential_boundary());

        // Supervisor enforcement is defense in depth, never a boundary.
        assert!(!EgressEnforcement::supervisor().is_credential_boundary());
    }
}
