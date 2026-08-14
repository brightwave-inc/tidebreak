//! Packaged desktop channel: production, debug, or staging.
//!
//! The three identities must not share a bundle id, product name, keychain
//! service, or deep-link scheme. Debug is selected by the build profile;
//! staging is selected by `TIDEBREAK_CHANNEL=staging` on a release build.
//! See [`docs/decisions/0016-desktop-staging-channel.md`].

pub const PRODUCTION_IDENTIFIER: &str = "io.brightwave.tidebreak";
pub const DEV_IDENTIFIER: &str = "io.brightwave.tidebreak.dev";
pub const STAGING_IDENTIFIER: &str = "io.brightwave.tidebreak.staging";

pub const PRODUCTION_PRODUCT_NAME: &str = "Tidebreak";
pub const DEV_PRODUCT_NAME: &str = "Tidebreak [dev]";
pub const STAGING_PRODUCT_NAME: &str = "Tidebreak [staging]";

pub const PRODUCTION_SCHEME: &str = "tidebreak";
pub const DEV_SCHEME: &str = "tidebreak-dev";
pub const STAGING_SCHEME: &str = "tidebreak-staging";

pub const DEV_KEYCHAIN_SERVICE: &str = "tidebreak.dev";
pub const STAGING_KEYCHAIN_SERVICE: &str = "tidebreak.staging";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Production,
    Dev,
    Staging,
}

impl Channel {
    pub const fn identifier(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_IDENTIFIER,
            Self::Dev => DEV_IDENTIFIER,
            Self::Staging => STAGING_IDENTIFIER,
        }
    }

    pub const fn product_name(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_PRODUCT_NAME,
            Self::Dev => DEV_PRODUCT_NAME,
            Self::Staging => STAGING_PRODUCT_NAME,
        }
    }

    pub const fn deep_link_scheme(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_SCHEME,
            Self::Dev => DEV_SCHEME,
            Self::Staging => STAGING_SCHEME,
        }
    }

    /// Keychain service that must not be shared with another channel.
    /// Production leaves this `None` so the server uses its default.
    pub const fn keychain_service(self) -> Option<&'static str> {
        match self {
            Self::Production => None,
            Self::Dev => Some(DEV_KEYCHAIN_SERVICE),
            Self::Staging => Some(STAGING_KEYCHAIN_SERVICE),
        }
    }

    pub fn accepts_scheme(self, scheme: &str) -> bool {
        // Debug still parses the production scheme so a `tidebreak://` link
        // typed at a dev window is honored. It never *registers* that
        // scheme; see `deep_link::install`.
        scheme == self.deep_link_scheme() || self == Self::Dev && scheme == PRODUCTION_SCHEME
    }
}

/// Channel this binary was compiled as.
///
/// Debug builds are always the red dev app, even if `TIDEBREAK_CHANNEL` is
/// set in the environment: a `cargo tauri dev` window must not pick up a
/// staging identifier from a leftover shell export. Staging is a
/// release-profile compile with `TIDEBREAK_CHANNEL=staging` baked in by
/// `build.rs`.
pub fn current() -> Channel {
    if cfg!(debug_assertions) {
        return Channel::Dev;
    }
    match option_env!("TIDEBREAK_CHANNEL") {
        Some("staging") => Channel::Staging,
        _ => Channel::Production,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_channels_do_not_share_identity() {
        let channels = [Channel::Production, Channel::Dev, Channel::Staging];
        let identifiers: Vec<_> = channels.iter().map(|c| c.identifier()).collect();
        let names: Vec<_> = channels.iter().map(|c| c.product_name()).collect();
        let schemes: Vec<_> = channels.iter().map(|c| c.deep_link_scheme()).collect();
        assert_eq!(identifiers.len(), unique(identifiers).len());
        assert_eq!(names.len(), unique(names).len());
        assert_eq!(schemes.len(), unique(schemes).len());
        assert_ne!(
            Channel::Dev.keychain_service(),
            Channel::Staging.keychain_service()
        );
        assert!(Channel::Production.keychain_service().is_none());
    }

    #[test]
    fn staging_does_not_honor_production_or_dev_schemes() {
        assert!(Channel::Staging.accepts_scheme(STAGING_SCHEME));
        assert!(!Channel::Staging.accepts_scheme(PRODUCTION_SCHEME));
        assert!(!Channel::Staging.accepts_scheme(DEV_SCHEME));
        assert!(Channel::Production.accepts_scheme(PRODUCTION_SCHEME));
        assert!(!Channel::Production.accepts_scheme(STAGING_SCHEME));
        assert!(!Channel::Production.accepts_scheme(DEV_SCHEME));
    }

    #[test]
    fn debug_builds_are_the_dev_channel() {
        assert_eq!(current() == Channel::Dev, cfg!(debug_assertions));
        assert_ne!(current(), Channel::Staging);
    }

    fn unique(values: Vec<&str>) -> Vec<&str> {
        let mut values = values;
        values.sort_unstable();
        values.dedup();
        values
    }
}
