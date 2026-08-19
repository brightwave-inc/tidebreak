//! What this client may ask of the host it runs on, and what it says when it
//! may not.
//!
//! Four authorities exist only on the machine this process runs on: the folder
//! broker, the client executor, native export, and computer use. All four reach
//! the user's own filesystem, screen, or input devices through credentials the
//! renderer never holds.
//!
//! When the client is attached to a remote machine (decision record 47), the
//! conversation lives somewhere else and none of those credentials apply to it.
//! The failure mode this module exists to prevent is the half-working one the
//! record names: a remote-attached client that quietly performs a folder
//! operation or a native export against the *wrong* host — the server's
//! filesystem, or this machine's, for a conversation on neither.
//!
//! So each authority refuses, and each refuses with its own stable reason, on
//! the pattern `output_writeback_authority_unavailable` set: a machine-readable
//! code that a client branches on, never prose. The renderer owns the copy.

use crate::remote::Attached;

/// A capability that exists only on the machine this process runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Authority {
    /// Connecting, listing, granting, and revoking host folders.
    FolderBroker,
    /// Running a tool call this client claimed on the host's behalf.
    ClientExecutor,
    /// Writing bytes to a destination chosen in a native save dialog.
    NativeExport,
    /// Observing and halting control of this computer's screen and input.
    ComputerUse,
}

/// The folder broker is not this client's to reach.
pub(crate) const FOLDER_BROKER_UNAVAILABLE: &str = "folder_broker_authority_unavailable";
/// The client executor is not this client's to reach.
pub(crate) const CLIENT_EXECUTOR_UNAVAILABLE: &str = "client_executor_authority_unavailable";
/// Native export is not this client's to reach.
pub(crate) const NATIVE_EXPORT_UNAVAILABLE: &str = "native_export_authority_unavailable";
/// Computer-use control is not this client's to reach.
pub(crate) const COMPUTER_USE_UNAVAILABLE: &str = "computer_use_authority_unavailable";

impl Authority {
    /// The stable reason this authority gives when it is unavailable.
    ///
    /// One string per authority, not one shared string: a client that can only
    /// learn "something host-shaped was refused" cannot tell the user which
    /// capability it lost, and the four are lost for the same cause but have
    /// four different consequences.
    pub(crate) const fn unavailable_reason(self) -> &'static str {
        match self {
            Self::FolderBroker => FOLDER_BROKER_UNAVAILABLE,
            Self::ClientExecutor => CLIENT_EXECUTOR_UNAVAILABLE,
            Self::NativeExport => NATIVE_EXPORT_UNAVAILABLE,
            Self::ComputerUse => COMPUTER_USE_UNAVAILABLE,
        }
    }
}

/// Refuse an authority the current attachment does not carry.
///
/// The error *is* the reason code, with no prose around it. These commands
/// otherwise return free-text `String` errors, so the renderer distinguishes a
/// refusal from a failure by comparing against the code rather than by reading
/// the message — see `hostAuthorityRefusal` in `ui/src/host.ts`.
pub(crate) fn require_local_authority(
    attachment: Option<&Attached>,
    authority: Authority,
) -> Result<(), String> {
    match attachment {
        None => Ok(()),
        Some(_) => Err(authority.unavailable_reason().to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attached() -> Attached {
        Attached {
            base_url: "https://machine.example.com".to_owned(),
            token: "token-value".to_owned(),
        }
    }

    const ALL: [Authority; 4] = [
        Authority::FolderBroker,
        Authority::ClientExecutor,
        Authority::NativeExport,
        Authority::ComputerUse,
    ];

    /// The decision-47 validation case: attached remotely, every host authority
    /// refuses, and refuses with its own reason rather than succeeding against
    /// the wrong host.
    #[test]
    fn every_authority_refuses_while_attached_remotely() {
        let attached = attached();
        for authority in ALL {
            let refusal = require_local_authority(Some(&attached), authority)
                .expect_err("a remote attachment carries no host authority");
            assert_eq!(refusal, authority.unavailable_reason());
        }
    }

    #[test]
    fn every_authority_is_available_on_the_local_machine() {
        for authority in ALL {
            assert!(require_local_authority(None, authority).is_ok());
        }
    }

    /// The codes are the contract. Written out rather than derived, so renaming
    /// a variant cannot silently change what a client sees.
    #[test]
    fn the_reasons_are_stable_and_distinct() {
        let reasons: Vec<&str> = ALL
            .iter()
            .map(|authority| authority.unavailable_reason())
            .collect();
        assert_eq!(
            reasons,
            vec![
                "folder_broker_authority_unavailable",
                "client_executor_authority_unavailable",
                "native_export_authority_unavailable",
                "computer_use_authority_unavailable",
            ]
        );
        let unique: std::collections::HashSet<&&str> = reasons.iter().collect();
        assert_eq!(unique.len(), reasons.len());
    }
}
