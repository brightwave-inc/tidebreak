//! The version handshake between a Tidebreak server and its clients.
//!
//! A server says which release it runs and which API level it serves. `GET
//! /version` answers with exactly that, and the `/healthz` and
//! `/auth/discovery` documents carry the same two keys beside their own. A
//! client reads the level before it relies on anything else and compares it
//! with the range it reads, so a version gap reads as "update Tidebreak"
//! rather than as a decode error later on.
//!
//! The level is not the release number. Raise [`API_LEVEL`] only when a
//! change breaks clients built before it: a removed or renamed field, a
//! changed meaning, or a route a client needs that moved. Adding a field, an
//! event, or a route does not raise it, because clients read tolerantly (see
//! `tidebreak_server::wire`).
//!
//! A server that predates this module answers `/version` with `404` and
//! leaves the keys out of the other two documents. Clients treat that as
//! compatible, so adding the handshake broke no attachment that worked before.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The API level this build serves, and the newest level its clients read.
pub const API_LEVEL: u32 = 1;

/// The oldest server API level this build's clients still read.
///
/// Raise it when a client stops handling an older server. Until then a new
/// client keeps working against every server at this level or above.
pub const MIN_API_LEVEL: u32 = 1;

/// What a server says about its own build.
//
// Readers ignore keys they do not know, so a key added here later does not
// break an older client. A plain comment, so the generated `wire.ts` does not
// carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ServerVersion {
    /// The Tidebreak release the server runs, such as `1.3.0`.
    pub version: String,
    /// The API level the server serves. Clients compare it with the range
    /// they read.
    pub api_level: u32,
}

impl ServerVersion {
    /// This build's own answer.
    pub fn current() -> Self {
        Self {
            version: tidebreak_core::VERSION.to_owned(),
            api_level: API_LEVEL,
        }
    }

    /// Read a `GET /version` answer, or a document that carries the same keys.
    ///
    /// Anything but a success with a JSON body naming both keys reads as
    /// `None`. A server that predates the route answers `404`, and a page in
    /// front of it may answer HTML; neither says anything about versions.
    ///
    /// A release a client could not print as it is reads as `None` too. The
    /// release ends up in a terminal and in the desktop's copy, so it has to
    /// be a plain release string (see [`is_printable_release`]). A server
    /// that sends anything else says nothing a client can use.
    pub fn from_answer(status: u16, body: &[u8]) -> Option<Self> {
        if !(200..300).contains(&status) {
            return None;
        }
        serde_json::from_slice::<Self>(body)
            .ok()
            .filter(|answer| is_printable_release(&answer.version))
    }
}

/// Whether a release string is short and plain enough to put in a sentence:
/// 1 to 32 characters from `0-9`, `A-Z`, `a-z`, `.`, `+`, and `-`.
///
/// The mobile app applies the same rule before it names a release. A string
/// outside it could carry terminal escapes, or words such as "from
/// https://example.com" that would turn "update to" into a lure.
pub fn is_printable_release(version: &str) -> bool {
    (1..=32).contains(&version.len())
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

/// How a client built from this source should treat a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Compatibility {
    /// The server serves a level this client reads, or it said nothing about
    /// versions because it predates the handshake.
    Compatible,
    /// The server serves a newer level than this client reads. Update the
    /// client.
    ClientTooOld { server_version: String },
    /// The server serves an older level than this client still reads. Update
    /// the server.
    ServerTooOld { server_version: String },
}

/// Compare what a server said with the range this build's clients read.
///
/// `None` means the server said nothing about versions, which only a server
/// from before the handshake does, so it is compatible.
pub fn compatibility(server: Option<&ServerVersion>) -> Compatibility {
    compatibility_within(server, MIN_API_LEVEL, API_LEVEL)
}

fn compatibility_within(server: Option<&ServerVersion>, min: u32, max: u32) -> Compatibility {
    match server {
        None => Compatibility::Compatible,
        Some(server) if server.api_level > max => Compatibility::ClientTooOld {
            server_version: server.version.clone(),
        },
        Some(server) if server.api_level < min => Compatibility::ServerTooOld {
            server_version: server.version.clone(),
        },
        Some(_) => Compatibility::Compatible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(api_level: u32) -> ServerVersion {
        ServerVersion {
            version: "1.3.0".to_owned(),
            api_level,
        }
    }

    #[test]
    fn a_level_inside_the_range_is_compatible() {
        assert_eq!(
            compatibility(Some(&ServerVersion::current())),
            Compatibility::Compatible
        );
        assert_eq!(
            compatibility_within(Some(&at(2)), 1, 3),
            Compatibility::Compatible
        );
    }

    #[test]
    fn a_newer_level_asks_for_a_newer_client() {
        assert_eq!(
            compatibility(Some(&at(API_LEVEL + 1))),
            Compatibility::ClientTooOld {
                server_version: "1.3.0".to_owned()
            }
        );
    }

    #[test]
    fn an_older_level_asks_for_a_newer_server() {
        assert_eq!(
            compatibility_within(Some(&at(1)), 2, 3),
            Compatibility::ServerTooOld {
                server_version: "1.3.0".to_owned()
            }
        );
    }

    /// The rule that keeps the handshake from breaking today's servers: one
    /// that says nothing about versions is not refused.
    #[test]
    fn a_server_that_predates_the_handshake_is_compatible() {
        assert_eq!(compatibility(None), Compatibility::Compatible);
        assert_eq!(ServerVersion::from_answer(404, b"not found"), None);
        assert_eq!(
            ServerVersion::from_answer(200, b"<!doctype html><title>app</title>"),
            None
        );
        assert_eq!(
            ServerVersion::from_answer(200, br#"{"mode":"local"}"#),
            None
        );
    }

    /// A release is printed to a terminal and put into the desktop's copy, so
    /// anything but a plain release string is no answer at all.
    #[test]
    fn a_release_a_client_could_not_print_is_no_answer() {
        for version in [
            "\u{1b}[1;31m9.4.0",
            "9.4.0 from https://evil.example",
            "9.4.0\nUpdate",
            "",
            "123456789012345678901234567890123",
        ] {
            let body = serde_json::json!({ "version": version, "api_level": 99 }).to_string();
            assert_eq!(
                ServerVersion::from_answer(200, body.as_bytes()),
                None,
                "{version:?}"
            );
        }
        for version in [
            "1.3.0",
            "0.0.0",
            "1.3.0-rc.1+build.5",
            tidebreak_core::VERSION,
        ] {
            assert!(is_printable_release(version), "{version:?}");
        }
    }

    /// Discovery and health documents carry the keys beside their own, and a
    /// newer server may add keys of its own; the reader takes what it knows.
    #[test]
    fn an_answer_is_read_from_any_document_that_carries_the_keys() {
        let body = br#"{"mode":"static_token","version":"1.3.0","api_level":1,"later":true}"#;
        assert_eq!(ServerVersion::from_answer(200, body), Some(at(1)));
        assert_eq!(ServerVersion::from_answer(500, body), None);
    }
}
